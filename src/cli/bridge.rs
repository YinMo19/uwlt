use anyhow::Result;
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
    dry_run: bool,
) -> Result<()> {
    let from = source.address(wallet)?;
    let service = bridge::Client::new()?;
    let quote = service.quote(
        source.bridge_chain()?,
        destination.bridge_chain()?,
        &from,
        recipient,
        amount,
    )?;
    show_quote(&quote);
    let mut fees = receipt::Fees::new(source);
    let result = match source.chain() {
        Chain::Tron(chain) => {
            let client = tron::Client::new(chain, rpc)?;
            let plan = client.prepare_contract(&from, &quote.call)?;
            println!("Approvals    {}", plan.approvals);
            println!(
                "Gas budget   <= {} {}",
                plan.fee_budget,
                quote.source.native_symbol()
            );
            if dry_run {
                println!("Not signed or broadcast.");
                return Ok(());
            }
            let key = authorize(wallet, &quote)?;
            client.execute_contract(&plan, &key, receipt::transaction, |stage, fee| {
                fees.record(stage, fee);
            })
        }
        Chain::Ethereum(chain) => {
            let client = ethereum::Client::new(chain, rpc, None)?;
            let plan = client.prepare_contract(&from, &quote.call)?;
            println!("Approvals    {}", plan.approvals);
            println!(
                "Gas budget   <= {} {}",
                plan.fee_budget,
                quote.source.native_symbol()
            );
            if dry_run {
                println!("Not signed or broadcast.");
                return Ok(());
            }
            let key = authorize(wallet, &quote)?;
            client.execute_contract(&plan, &key, receipt::transaction, |stage, fee| {
                fees.record(stage, fee);
            })
        }
    };
    let txid = fees.finish(result)?;
    println!("Source transaction confirmed; destination delivery is pending.");
    println!(
        "Track: uwlt --network {} bridge-status {txid}",
        source.label()
    );
    Ok(())
}

pub(super) fn status(source: Network, txid: &str) -> Result<()> {
    let status = bridge::Client::new()?.status(source.bridge_chain()?, txid)?;
    println!("{txid}");
    println!("State  {}", status.state);
    if let Some(destination) = status.destination_tx {
        println!("Destination transaction  {destination}");
    }
    if let Some(details) = status.details {
        println!("{details}");
    }
    Ok(())
}

fn show_quote(quote: &bridge::Quote) {
    println!("Provider     Allbridge Core");
    println!("Protocol     {}", quote.messenger);
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
    println!("Bridge fee   {}", quote.service_fee);
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
            "Bridge {} {} from {} to {} on {}",
            quote.amount,
            quote.source_symbol,
            quote.source.label(),
            quote.recipient,
            quote.destination.label()
        ),
    )
}
