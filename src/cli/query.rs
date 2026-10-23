use anyhow::Result;

use super::{
    network::{Chain, Network},
    receipt,
};
use crate::{ethereum, solana, tron};

pub(super) fn balance<'a>(
    network: Network,
    rpc: Option<&str>,
    accounts: impl IntoIterator<Item = Result<(Option<&'a str>, String)>>,
) -> Result<()> {
    let mut accounts = accounts.into_iter().peekable();
    if accounts.peek().is_none() {
        println!("No local wallets. Use `uwlt create` or `uwlt sync`.");
        return Ok(());
    }
    match network.chain() {
        Chain::Tron => {
            let client = tron::Client::new(rpc)?;
            for (index, account) in accounts.enumerate() {
                let (name, address) = account?;
                let balance = client.balance(&address)?;
                account_heading(index, name, &address);
                println!("USDT  {}", tron::format_amount(balance.usdt));
                println!("TRX   {}", tron::format_amount(balance.trx));
            }
        }
        Chain::Ethereum(network) => {
            let client = ethereum::Client::new(network, rpc)?;
            for (index, account) in accounts.enumerate() {
                let (name, address) = account?;
                let balance = client.balance(&address)?;
                account_heading(index, name, &address);
                println!("{:<5} {}", client.token_symbol(), balance.usdt);
                println!("{:<5} {}", client.native_symbol(), balance.native);
            }
        }
        Chain::Solana => {
            let client = solana::Client::new(rpc)?;
            for (index, account) in accounts.enumerate() {
                let (name, address) = account?;
                let balance = client.balance(&address)?;
                account_heading(index, name, &address);
                println!("{:<5} {}", client.token_symbol(), balance.usdt);
                println!("{:<5} {}", client.native_symbol(), balance.native);
            }
        }
    }
    Ok(())
}

fn account_heading(index: usize, name: Option<&str>, address: &str) {
    if index > 0 {
        println!();
    }
    match name {
        Some(name) => println!("{name}  {address}"),
        None => println!("{address}"),
    }
}

pub(super) fn status(network: Network, rpc: Option<&str>, txid: &str) -> Result<()> {
    match network.chain() {
        Chain::Tron => {
            let status = tron::Client::new(rpc)?.status(txid)?;
            receipt::tron(network, txid, &status, None)
        }
        Chain::Ethereum(chain) => {
            let status = ethereum::Client::new(chain, rpc)?.status(txid)?;
            receipt::ethereum(network, txid, &status, None)
        }
        Chain::Solana => {
            let status = solana::Client::new(rpc)?.status(txid)?;
            receipt::solana(network, txid, &status, None)
        }
    }
}
