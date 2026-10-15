mod bridge;
mod fees;
mod network;
mod query;
mod receipt;
mod swap;
mod transfer;

use anyhow::{Result, ensure};
use clap::{CommandFactory, Parser, Subcommand};
use std::{
    io::{self, Write},
    path::PathBuf,
};
use zeroize::Zeroizing;

use crate::{
    core, vault,
    wallet::{self, Store, Wallet},
};
use network::Network;
use transfer::Transfer;

#[derive(Parser)]
#[command(version, about = "USDT wallet for TRON, Ethereum and Polygon")]
pub struct Cli {
    #[arg(
        short,
        long,
        global = true,
        default_value = "main",
        help = "Wallet name"
    )]
    wallet: String,
    #[arg(long, global = true, value_enum, help = "Network; prompt when omitted")]
    network: Option<Network>,
    #[arg(long, global = true, env = "UWLT_RPC", help = "HTTP RPC endpoint")]
    rpc: Option<String>,
    #[arg(long, global = true, help = "Sepolia test-token contract")]
    token: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a wallet; prompt for a name when omitted
    Create { name: Option<String> },
    /// Import a private key from hidden input or a backup file
    Import {
        #[arg(default_value = "main")]
        name: String,
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// List local wallets
    List,
    /// Migrate keys to iCloud Keychain and refresh local wallets from downloaded items
    Sync,
    /// Print the receiving address on the selected network
    Address,
    /// Show token and native-coin balances for a wallet or address
    Balance {
        address: Option<String>,
        #[arg(
            long,
            conflicts_with_all = ["address", "wallet"],
            help = "Show every local wallet on the selected network"
        )]
        all: bool,
    },
    /// Send USDT, or use --trx / --eth / --pol for the native coin
    Send(Transfer),
    /// Exchange USDT for the selected network's native coin
    Swap(swap::Swap),
    /// Show current network fee rates
    Fees {
        #[arg(long, help = "Query every supported network using its default RPC")]
        all: bool,
    },
    /// Query transaction execution and confirmation
    Status { txid: String },
    /// Track an Allbridge transfer to its destination
    BridgeStatus { txid: String },
    /// Show the wallet name and private key
    Key,
    /// Export the private key to a new owner-only file
    Backup { path: PathBuf },
    /// Remove a wallet from iCloud Keychain on all devices; funds remain on chain
    Remove { name: String },
    /// Check Wallet Core and Touch ID availability
    Doctor,
    /// Generate shell completions
    Completion {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

impl Cli {
    pub fn run(self) -> Result<()> {
        let select_network = || -> Result<Network> {
            let network = match self.network {
                Some(network) => network,
                None => Network::prompt()?,
            };
            ensure!(
                self.token.is_none() || matches!(network, Network::Sepolia),
                "--token is only used with Sepolia"
            );
            Ok(network)
        };
        match self.command {
            Command::Completion { shell } => {
                clap_complete::generate(shell, &mut Self::command(), "uwlt", &mut io::stdout());
            }
            Command::Doctor => {
                let key = core::generate()?;
                core::address(&key)?;
                println!("Wallet Core: available");
                vault::available()?;
                println!("Touch ID: available");
            }
            Command::Create { name } => {
                let name = match name {
                    Some(name) => name,
                    None => prompt_name()?,
                };
                let mut store = Store::open()?;
                store.check_new(&name)?;
                let key = core::generate()?;
                add_wallet(&mut store, name, &key)?;
            }
            Command::Import { name, file } => {
                let mut store = Store::open()?;
                store.check_new(&name)?;
                let key = wallet::read_key(file.as_deref())?;
                add_wallet(&mut store, name, &key)?;
            }
            Command::List => {
                let network = select_network()?;
                let store = Store::open()?;
                for (name, wallet) in store.entries() {
                    println!("{name}\t{}", network.address(wallet)?);
                }
            }
            Command::Sync => {
                let network = select_network()?;
                let mut store = Store::open()?;
                let wallets: Vec<_> = store
                    .entries()
                    .map(|(name, wallet)| vault::SyncWallet {
                        id: wallet.id.clone(),
                        name: name.clone(),
                        address: wallet.address.clone(),
                    })
                    .collect();
                let wallets = vault::sync(&wallets)?;
                let count = wallets.len();
                store.replace(wallets)?;
                for (name, wallet) in store.entries() {
                    println!("{name}\t{}", network.address(wallet)?);
                }
                println!(
                    "{count} wallet(s) available in iCloud Keychain; delivery to other devices is managed by macOS."
                );
            }
            Command::Address => {
                let network = select_network()?;
                let store = Store::open()?;
                println!("{}", network.address(store.get(&self.wallet)?)?);
            }
            Command::Balance { address, all } => {
                let network = select_network()?;
                if all {
                    let store = Store::open()?;
                    query::balance(
                        network,
                        self.rpc.as_deref(),
                        self.token.as_deref(),
                        store.entries().map(|(name, wallet)| {
                            network
                                .address(wallet)
                                .map(|address| (Some(name.as_str()), address))
                        }),
                    )?;
                } else {
                    let account = match address {
                        Some(address) => (None, address),
                        None => (
                            Some(self.wallet.as_str()),
                            network.address(Store::open()?.get(&self.wallet)?)?,
                        ),
                    };
                    query::balance(
                        network,
                        self.rpc.as_deref(),
                        self.token.as_deref(),
                        std::iter::once(Ok(account)),
                    )?;
                }
            }
            Command::Swap(swap) => {
                let network = select_network()?;
                let store = Store::open()?;
                swap.run(
                    network,
                    self.rpc.as_deref(),
                    self.token.as_deref(),
                    store.get(&self.wallet)?,
                )?;
            }
            Command::Fees { all } => {
                if all {
                    fees::all()?;
                } else {
                    fees::run(
                        select_network()?,
                        self.rpc.as_deref(),
                        self.token.as_deref(),
                    )?;
                }
            }
            Command::Send(mut transfer) => {
                let network = select_network()?;
                let store = Store::open()?;
                transfer.resolve_recipient(network, &store)?;
                println!("Wallet   {}", self.wallet);
                transfer.run(
                    network,
                    self.rpc.as_deref(),
                    self.token.as_deref(),
                    store.get(&self.wallet)?,
                )?;
            }
            Command::BridgeStatus { txid } => {
                bridge::status(select_network()?, &txid)?;
            }
            Command::Status { txid } => {
                query::status(
                    select_network()?,
                    self.rpc.as_deref(),
                    self.token.as_deref(),
                    &txid,
                )?;
            }
            Command::Key => {
                let store = Store::open()?;
                let wallet = store.get(&self.wallet)?;
                let key =
                    vault::load(&wallet.id, &format!("Show private key for {}", self.wallet))?;
                let encoded = Zeroizing::new(hex::encode(key.as_slice()));
                writeln!(io::stdout().lock(), "{}  {}", self.wallet, &*encoded)?;
            }
            Command::Backup { path } => {
                ensure!(!path.exists(), "backup path already exists");
                let store = Store::open()?;
                let wallet = store.get(&self.wallet)?;
                let key = vault::load(&wallet.id, &format!("Back up wallet {}", self.wallet))?;
                wallet::write_backup(&path, &key)?;
                println!("Private key saved to {} (mode 0600).", path.display());
            }
            Command::Remove { name } => {
                let mut store = Store::open()?;
                let wallet = store.get(&name)?;
                if !confirm_removal(&name)? {
                    println!("Cancelled.");
                    return Ok(());
                }
                vault::delete(
                    &wallet.id,
                    &format!("Remove wallet {name} from iCloud Keychain on all devices"),
                )?;
                store.remove(&name)?;
                println!("Removed {name} from iCloud Keychain.");
            }
        }
        Ok(())
    }
}

fn add_wallet(store: &mut Store, name: String, key: &[u8; 32]) -> Result<()> {
    let wallet = Wallet {
        address: core::address(key)?,
        id: uuid::Uuid::new_v4().to_string(),
    };
    vault::save(
        &wallet.id,
        &name,
        &wallet.address,
        key,
        &format!("Save wallet {name}"),
    )?;
    store.insert(name.clone(), wallet)?;
    let wallet = store.get(&name)?;
    println!("{name}");
    println!("TRON  {}", wallet.address);
    println!("EVM   {} (Ethereum / Polygon)", wallet.ethereum_address()?);
    println!("Back up with: uwlt --wallet {name} backup <file>");
    Ok(())
}

fn confirm_removal(name: &str) -> Result<bool> {
    eprintln!("Remove {name} from iCloud Keychain on all devices.");
    eprint!("Type confirm to delete: ");
    io::stderr().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim_end_matches(['\r', '\n']) == "confirm")
}

fn prompt_name() -> Result<String> {
    let mut name = String::new();
    loop {
        eprint!("Wallet name (a-z): ");
        io::stderr().flush()?;
        name.clear();
        ensure!(
            io::stdin().read_line(&mut name)? != 0,
            "no wallet name provided"
        );
        name.truncate(name.trim_end().len());
        match wallet::validate_name(&name) {
            Ok(()) => return Ok(name),
            Err(error) => eprintln!("{error}"),
        }
    }
}
