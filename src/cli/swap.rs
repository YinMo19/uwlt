use anyhow::{Result, ensure};
use clap::Args;
use std::io::{self, Write};
use zeroize::Zeroizing;

use super::{
    network::{Chain, Network},
    receipt,
};
use crate::{bridge, ethereum, solana, swap::Summary, tron, vault, wallet::Wallet};

#[derive(Args)]
pub(super) struct Swap {
    /// Amount of USDT/USDT0 to exchange
    amount: String,
    #[arg(long = "slippage", default_value = "0.5", value_parser = slippage,
        help = "Maximum slippage in percent")]
    slippage_bps: u16,
    #[arg(long, help = "Show the quote without signing or broadcasting")]
    dry_run: bool,
}

impl Swap {
    pub fn run(self, network: Network, rpc: Option<&str>, wallet: &Wallet) -> Result<()> {
        let from = network.address(wallet)?;
        let chain = network.bridge_chain();
        let quote = bridge::Client::new()?.quote_swap(
            chain,
            &from,
            &self.amount,
            self.slippage_bps,
            self.dry_run,
        )?;
        let mut summary = Summary {
            venue: "MetaMask routing service",
            input: quote.input.clone(),
            expected: quote.expected.clone(),
            minimum: quote.minimum.clone(),
            fee_budget: String::new(),
            approvals: 0,
            token: if network == Network::Polygon {
                "USDT0"
            } else {
                "USDT"
            },
            native: chain.native_symbol(),
        };
        let mut fees = receipt::Fees::new(network);
        let result = match (network.chain(), &quote.transaction) {
            (Chain::Tron, bridge::NativeSwapTransaction::Contract(call)) => {
                let client = tron::Client::new(rpc)?;
                let plan = client.prepare_contract(&from, call)?;
                summary.fee_budget = plan.fee_budget.clone();
                summary.approvals = plan.approvals;
                preview(network, &from, &summary, self.slippage_bps, &quote);
                if self.dry_run {
                    println!(
                        "Indicative quote; execution is simulated stage by stage before signing."
                    );
                    println!("Not signed or broadcast.");
                    return Ok(());
                }
                let key = authorize(network, wallet, &summary)?;
                quote.ensure_fresh()?;
                client.execute_contract(&plan, &key, receipt::transaction, |stage, fee| {
                    fees.record(stage, fee);
                })
            }
            (Chain::Ethereum(chain), bridge::NativeSwapTransaction::Contract(call)) => {
                let client = ethereum::Client::new(chain, rpc)?;
                let plan = client.prepare_contract(&from, call)?;
                summary.fee_budget = plan.fee_budget.clone();
                summary.approvals = plan.approvals;
                preview(network, &from, &summary, self.slippage_bps, &quote);
                if self.dry_run {
                    println!(
                        "Indicative quote; execution is simulated stage by stage before signing."
                    );
                    println!("Not signed or broadcast.");
                    return Ok(());
                }
                let key = authorize(network, wallet, &summary)?;
                quote.ensure_fresh()?;
                client.execute_contract(&plan, &key, receipt::transaction, |stage, fee| {
                    fees.record(stage, fee);
                })
            }
            (Chain::Solana, bridge::NativeSwapTransaction::Solana(route)) => {
                let client = solana::Client::new(rpc)?;
                let prepared = client.prepare_swap(route)?;
                summary.fee_budget = prepared.summary.fee_budget.clone();
                preview(network, &from, &summary, self.slippage_bps, &quote);
                if self.dry_run {
                    println!("Transaction simulated; native SOL output verified.");
                    println!("Not signed or broadcast.");
                    return Ok(());
                }
                let key = authorize(network, wallet, &summary)?;
                quote.ensure_fresh()?;
                client.execute_swap(&prepared, &key, receipt::transaction, |stage, fee| {
                    fees.record(stage, alloy::primitives::U256::from(fee));
                })
            }
            _ => anyhow::bail!("MetaMask swap transaction does not match the selected network"),
        };
        fees.finish(result)?;
        println!("Swap confirmed.");
        Ok(())
    }
}

pub(super) fn slippage(value: &str) -> Result<u16> {
    let units = tron::parse_amount(value)?;
    ensure!(
        units < 100_000_000 && units % 10_000 == 0,
        "slippage must be below 100%, with at most two decimal places"
    );
    Ok((units / 10_000) as u16)
}

fn preview(
    network: Network,
    recipient: &str,
    quote: &Summary,
    slippage_bps: u16,
    provider: &bridge::NativeSwapQuote,
) {
    println!("Network     {}", network.label());
    println!("Venue       {}", quote.venue);
    println!("Route       {}", provider.route);
    println!("Recipient   {recipient}");
    println!("Pay         {} {}", quote.input, quote.token);
    println!("Expected    {} {}", quote.expected, quote.native);
    println!("Minimum     {} {}", quote.minimum, quote.native);
    println!(
        "Slippage    {}.{:02}%",
        slippage_bps / 100,
        slippage_bps % 100
    );
    println!("Approvals   {}", quote.approvals);
    println!("Fee budget  <= {} {}", quote.fee_budget, quote.native);
    println!("Budget includes network fees and any refundable account rent, not actual cost.");
    for fee in &provider.fees {
        println!("Fees        {fee}");
    }
}

fn authorize(network: Network, wallet: &Wallet, quote: &Summary) -> Result<Zeroizing<[u8; 32]>> {
    io::stdout().flush()?;
    vault::load(
        &wallet.id,
        &format!(
            "Swap {} {} for at least {} {} on {}",
            quote.input,
            quote.token,
            quote.minimum,
            quote.native,
            network.label()
        ),
    )
}
