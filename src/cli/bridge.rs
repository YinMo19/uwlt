use anyhow::{Context, Result};
use std::io::{self, Write};
use zeroize::Zeroizing;

use super::{
    network::{Chain, Network},
    receipt,
};
use crate::{bridge, ethereum, tron, vault, wallet::Wallet};

pub(super) fn send(
    source: Network,
    destination: Network,
    rpc: Option<&str>,
    wallet: &Wallet,
    recipient: &str,
    amount: &str,
    slippage_bps: u16,
    dry_run: bool,
) -> Result<()> {
    let source_chain = source.bridge_chain()?;
    let destination_chain = destination.bridge_chain()?;
    let from = source.address(wallet)?;
    let service = bridge::Client::new()?;
    let quote = service.quote(
        source_chain,
        destination_chain,
        &from,
        recipient,
        amount,
        slippage_bps,
        dry_run,
    )?;
    show_quote(&quote);
    if dry_run {
        println!(
            "Indicative quote only; balances and executable gas budget are checked before signing."
        );
        println!("Not signed or broadcast.");
        return Ok(());
    }
    let call = quote
        .call
        .as_ref()
        .context("MetaMask returned no transaction")?;
    println!("Contract     {}", call.to);
    if let Some(spender) = &call.spender {
        println!("Spender      {spender}");
        println!(
            "Allowance    {} {}",
            ethereum::format_amount(call.amount, 6),
            quote.source_symbol
        );
    }
    println!(
        "Native value {}",
        receipt::native_amount(source, call.value)
    );
    let mut fees = receipt::Fees::new(source);
    // Print the recovery command at broadcast, not after receipt polling: a
    // timeout or process interruption must not hide the tracking identifiers.
    let submitted = |stage: &str, txid: &str| {
        receipt::transaction(stage, txid);
        if stage == "bridge" {
            println!(
                "Track: {}",
                recovery_command(
                    source,
                    destination,
                    &quote.bridge_id,
                    &quote.request_id,
                    txid
                )
            );
        }
    };
    let result = match source.chain() {
        Chain::Tron(chain) => {
            let client = tron::Client::new(chain, rpc)?;
            let plan = client.prepare_contract(&from, call)?;
            println!("Approvals    {}", plan.approvals);
            println!(
                "Gas budget   <= {} {} (includes native value)",
                plan.fee_budget,
                quote.source.native_symbol()
            );
            let key = authorize(wallet, &quote)?;
            quote.ensure_fresh()?;
            client.execute_contract(&plan, &key, submitted, |stage, fee| {
                fees.record(stage, fee);
            })
        }
        Chain::Ethereum(chain) => {
            let client = ethereum::Client::new(chain, rpc, None)?;
            let plan = client.prepare_contract(&from, call)?;
            println!("Approvals    {}", plan.approvals);
            println!(
                "Gas budget   <= {} {} (includes native value)",
                plan.fee_budget,
                quote.source.native_symbol()
            );
            let key = authorize(wallet, &quote)?;
            quote.ensure_fresh()?;
            client.execute_contract(&plan, &key, submitted, |stage, fee| {
                fees.record(stage, fee);
            })
        }
    };
    fees.finish(result)?;
    println!("Source transaction confirmed; destination delivery is pending.");
    Ok(())
}

pub(super) fn status(
    source: Network,
    destination: Network,
    bridge_id: &str,
    request_id: &str,
    txid: &str,
) -> Result<()> {
    let source = source.bridge_chain()?;
    let destination = destination.bridge_chain()?;
    let status = bridge::Client::new()?.status(source, destination, bridge_id, request_id, txid)?;
    println!("Provider MetaMask routing service");
    println!("Bridge   {bridge_id}");
    println!("Request  {request_id}");
    println!("From     {}", source.label());
    println!("To       {}", destination.label());
    println!("Source   {txid}");
    println!("State    {}", status.state);
    if let Some(destination) = status.destination_tx {
        println!("Destination transaction  {destination}");
    }
    if let Some(details) = status.details {
        println!("{details}");
    }
    Ok(())
}

fn show_quote(quote: &bridge::Quote) {
    println!("Provider     MetaMask routing service (local uwlt signing)");
    println!("Route        {}", quote.messenger);
    println!("Request      {}", quote.request_id);
    println!("Bridge ID    {}", quote.bridge_id);
    println!("From         {} ({})", quote.sender, quote.source.label());
    println!(
        "To           {} ({})",
        quote.recipient,
        quote.destination.label()
    );
    println!("Pay          {} {}", quote.amount, quote.source_symbol);
    println!(
        "Expected     {} {}",
        quote.expected, quote.destination_symbol
    );
    println!(
        "Minimum      {} {}",
        quote.minimum, quote.destination_symbol
    );
    println!(
        "Slippage     {}.{:02}%",
        quote.slippage_bps / 100,
        quote.slippage_bps % 100
    );
    for fee in &quote.fees {
        println!("Quote fee    {fee}");
    }
    if let Some(seconds) = quote.estimated_seconds {
        println!("Delivery     ~{seconds} seconds");
    }
}

fn authorize(wallet: &Wallet, quote: &bridge::Quote) -> Result<Zeroizing<[u8; 32]>> {
    quote.ensure_fresh()?;
    io::stdout().flush()?;
    vault::load(
        &wallet.id,
        &format!(
            "MetaMask route, uwlt local signing: bridge {} {} from {} to {} on {}; minimum {} {}",
            quote.amount,
            quote.source_symbol,
            quote.source.label(),
            quote.recipient,
            quote.destination.label(),
            quote.minimum,
            quote.destination_symbol
        ),
    )
}

fn recovery_command(
    source: Network,
    destination: Network,
    bridge_id: &str,
    request_id: &str,
    txid: &str,
) -> String {
    // These identifiers come from a remote provider: quote them as shell arguments.
    let quoted = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    format!(
        "uwlt --network {} bridge-status {} --to-network {} --bridge-id {} --request-id {}",
        source.label(),
        quoted(txid),
        destination.label(),
        quoted(bridge_id),
        quoted(request_id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_command_contains_every_tracking_argument_and_quotes_remote_values() {
        assert_eq!(
            recovery_command(
                Network::Mainnet,
                Network::Ethereum,
                "relay",
                "req'$(touch nope)",
                "abcd"
            ),
            "uwlt --network mainnet bridge-status 'abcd' --to-network ethereum --bridge-id 'relay' --request-id 'req'\\''$(touch nope)'"
        );
    }
}
