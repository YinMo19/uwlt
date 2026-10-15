use anyhow::{Context, Result, ensure};
use clap::Args;
use std::{
    io::{self, Write},
    time::{SystemTime, UNIX_EPOCH},
};

use super::{
    network::{Chain, Network},
    receipt,
};
use crate::{
    core, ethereum, tron, vault,
    wallet::{Store, Wallet},
};

#[derive(Args)]
pub(super) struct Transfer {
    /// Amount of USDT/USDT0, or native coin when its flag is set
    amount: String,
    #[arg(long, help = "Recipient address or local wallet name")]
    to: String,
    #[arg(
        long,
        value_enum,
        help = "Destination network for an Allbridge transfer"
    )]
    to_network: Option<Network>,
    #[arg(long, group = "native", help = "Send native TRX on TRON")]
    trx: bool,
    #[arg(long, group = "native", help = "Send native ETH on Ethereum")]
    eth: bool,
    #[arg(long, group = "native", help = "Send native POL on Polygon")]
    pol: bool,
    #[arg(long, help = "TRON energy budget in TRX (default: 100)")]
    fee_limit: Option<String>,
    #[arg(long, help = "Preview the transfer without signing or broadcasting")]
    dry_run: bool,
}

impl Transfer {
    pub fn resolve_recipient(&mut self, network: Network, store: &Store) -> Result<()> {
        if let Some(wallet) = store.find(&self.to) {
            self.to = self.to_network.unwrap_or(network).address(wallet)?;
        } else if !self.to.is_empty() && self.to.bytes().all(|byte| byte.is_ascii_lowercase()) {
            store.get(&self.to)?;
        }
        Ok(())
    }

    pub fn run(
        self,
        network: Network,
        rpc: Option<&str>,
        token: Option<&str>,
        wallet: &Wallet,
    ) -> Result<()> {
        if let Some(destination) = self
            .to_network
            .filter(|destination| *destination != network)
        {
            ensure!(
                !self.trx && !self.eth && !self.pol,
                "Allbridge transfers use USDT"
            );
            ensure!(
                self.fee_limit.is_none(),
                "--fee-limit applies to same-chain TRON transfers"
            );
            return super::bridge::send(
                network,
                destination,
                rpc,
                wallet,
                &self.to,
                &self.amount,
                self.dry_run,
            );
        }
        match network.chain() {
            Chain::Tron(chain) => {
                ensure!(!self.eth && !self.pol, "use --trx for TRON transfers");
                self.tron(network, tron::Client::new(chain, rpc)?, wallet)
            }
            Chain::Ethereum(chain) => {
                match chain {
                    ethereum::Network::Polygon => {
                        ensure!(!self.trx && !self.eth, "use --pol for Polygon transfers");
                    }
                    _ => {
                        ensure!(!self.trx && !self.pol, "use --eth for Ethereum transfers");
                    }
                }
                ensure!(
                    self.fee_limit.is_none(),
                    "--fee-limit applies to TRON energy only"
                );
                self.ethereum(network, ethereum::Client::new(chain, rpc, token)?, wallet)
            }
        }
    }

    fn tron(self, network: Network, client: tron::Client, wallet: &Wallet) -> Result<()> {
        let amount = positive_amount(&self.amount)?;
        let fee_limit = positive_amount(self.fee_limit.as_deref().unwrap_or("100"))?;
        let asset = if self.trx {
            tron::Asset::Trx
        } else {
            tron::Asset::Usdt
        };
        let prepared = client.prepare(&wallet.address, &self.to, amount, asset, fee_limit)?;
        let fee = client.fee_for(&prepared, &wallet.address, &self.to, asset)?;
        println!("Network  {}", network.label());
        println!("From     {}", wallet.address);
        println!("To       {}", self.to);
        println!("Amount   {} {asset}", tron::format_amount(amount));
        if !self.trx {
            println!(
                "Energy limit  {} TRX (bandwidth charged separately)",
                tron::format_amount(fee_limit)
            );
        }
        if fee.energy != 0 {
            println!(
                "Energy   {} ({} TRX)",
                fee.energy,
                tron::format_amount(fee.energy_sun)
            );
        }
        println!(
            "Bandwidth  {} bytes ({} TRX)",
            fee.bandwidth,
            tron::format_amount(fee.bandwidth_sun)
        );
        if fee.activation_sun != 0 {
            println!(
                "Activation  {} TRX",
                tron::format_amount(fee.activation_sun)
            );
        }
        println!("Estimated fee  {} TRX", tron::format_amount(fee.total_sun));
        if self.dry_run {
            println!("Not signed or broadcast.");
            return Ok(());
        }
        io::stdout().flush()?;
        let reason = format!(
            "Send {} {asset} to {}",
            tron::format_amount(amount),
            self.to
        );
        let key = vault::load(&wallet.id, &reason)?;
        ensure!(
            core::address(&key)? == wallet.address,
            "private key does not match wallet address"
        );
        let expiration = prepared.transaction["raw_data"]["expiration"]
            .as_u64()
            .context("transaction has no expiration")?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        ensure!(
            u128::from(expiration) > now,
            "transaction expired; run send again"
        );
        let signed = core::sign(&key, &prepared.transaction)?;
        drop(key);
        let txid = signed["txID"]
            .as_str()
            .context("signed transaction has no txID")?;
        println!("Transaction  {txid}");
        io::stdout().flush()?;
        client.broadcast(&signed)?;
        println!("Submitted; waiting for confirmation...");
        let status = client.wait_for_receipt(txid)?;
        let transfer = crate::transaction::Transfer {
            recipient: self.to,
            amount: tron::format_amount(amount),
            symbol: if self.trx { "TRX" } else { "USDT" },
        };
        println!();
        receipt::tron(network, txid, &status, Some((&wallet.address, &transfer)))
    }

    fn ethereum(self, network: Network, client: ethereum::Client, wallet: &Wallet) -> Result<()> {
        let asset = if self.eth || self.pol {
            ethereum::Asset::Native
        } else {
            ethereum::Asset::Usdt
        };
        let from = wallet.ethereum_address()?;
        let prepared = client.prepare(&from, &self.to, &self.amount, asset)?;
        println!("Network  {}", network.label());
        println!("From     {}", prepared.from);
        println!("To       {}", prepared.to);
        println!("Amount   {} {}", prepared.amount, prepared.symbol);
        println!("Fee cap  {} {}", prepared.fee, client.native_symbol());
        if self.dry_run {
            println!("Not signed or broadcast.");
            return Ok(());
        }
        io::stdout().flush()?;
        let reason = format!(
            "Send {} {} to {}",
            prepared.amount, prepared.symbol, prepared.to
        );
        let key = vault::load(&wallet.id, &reason)?;
        let signed = ethereum::sign(&prepared, &key)?;
        drop(key);
        println!("Transaction  {}", signed.txid);
        io::stdout().flush()?;
        client.broadcast(&signed)?;
        println!("Submitted; waiting for confirmation...");
        let status = client.wait_for_receipt(&signed.txid)?;
        let transfer = crate::transaction::Transfer {
            recipient: prepared.to,
            amount: prepared.amount,
            symbol: match asset {
                ethereum::Asset::Native => client.native_symbol(),
                ethereum::Asset::Usdt => client.token_symbol(),
            },
        };
        println!();
        receipt::ethereum(
            network,
            &signed.txid,
            &status,
            Some((&prepared.from, &transfer)),
        )
    }
}

fn positive_amount(value: &str) -> Result<u64> {
    let amount = tron::parse_amount(value)?;
    ensure!(amount > 0, "amount must be greater than zero");
    Ok(amount)
}
