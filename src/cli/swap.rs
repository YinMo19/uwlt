use anyhow::{Result, ensure};
use clap::Args;
use std::io::{self, Write};
use zeroize::Zeroizing;

use super::{
    network::{Chain, Network},
    receipt,
};
use crate::{ethereum, swap::Summary, tron, vault, wallet::Wallet};

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
    pub fn run(
        self,
        network: Network,
        rpc: Option<&str>,
        token: Option<&str>,
        wallet: &Wallet,
    ) -> Result<()> {
        let from = network.address(wallet)?;
        let mut fees = receipt::Fees::new(network);
        let result = match network.chain() {
            Chain::Tron(chain) => {
                let client = tron::Client::new(chain, rpc)?;
                let quote = client.quote_swap(&from, &self.amount, self.slippage_bps)?;
                preview(network, &from, &quote.summary, self.slippage_bps);
                if self.dry_run {
                    return Ok(());
                }
                let key = authorize(network, wallet, &quote.summary)?;
                client.execute_swap(&quote, &key, receipt::transaction, |stage, fee| {
                    fees.record(stage, fee);
                })
            }
            Chain::Ethereum(chain) => {
                let client = ethereum::Client::new(chain, rpc, token)?;
                let quote = client.quote_swap(&from, &self.amount, self.slippage_bps)?;
                preview(network, &from, &quote.summary, self.slippage_bps);
                if self.dry_run {
                    return Ok(());
                }
                let key = authorize(network, wallet, &quote.summary)?;
                client.execute_swap(&quote, &key, receipt::transaction, |stage, fee| {
                    fees.record(stage, fee);
                })
            }
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

fn preview(network: Network, recipient: &str, quote: &Summary, slippage_bps: u16) {
    println!("Network     {}", network.label());
    println!("Venue       {}", quote.venue);
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
