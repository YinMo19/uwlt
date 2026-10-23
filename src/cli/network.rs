use std::io::{self, Write};

use anyhow::{Result, bail};
use clap::ValueEnum;

use crate::{ethereum, solana, tron, wallet::Wallet};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum Network {
    Mainnet,
    Nile,
    Ethereum,
    Sepolia,
    Polygon,
    Solana,
    SolanaDevnet,
}

pub(super) enum Chain {
    Tron(tron::Network),
    Ethereum(ethereum::Network),
    Solana(solana::Network),
}

impl Network {
    pub fn prompt() -> Result<Self> {
        let networks = Self::value_variants();
        eprintln!("Select network:");
        for (index, network) in networks.iter().enumerate() {
            let description = match network {
                Self::Mainnet => "TRON mainnet",
                Self::Nile => "TRON testnet",
                Self::Ethereum => "Ethereum mainnet",
                Self::Sepolia => "Ethereum testnet",
                Self::Polygon => "Polygon PoS",
                Self::Solana => "Solana mainnet",
                Self::SolanaDevnet => "Solana devnet",
            };
            eprintln!("  {}. {} ({description})", index + 1, network.label());
        }
        let mut input = String::new();
        loop {
            eprint!("Network (number or name): ");
            io::stderr().flush()?;
            input.clear();
            if io::stdin().read_line(&mut input)? == 0 || input.trim().is_empty() {
                bail!("network selection cancelled; specify --network to select explicitly");
            }
            let input = input.trim();
            let network = input
                .parse::<usize>()
                .ok()
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| networks.get(index).copied())
                .or_else(|| Self::from_str(input, false).ok());
            if let Some(network) = network {
                return Ok(network);
            }
            eprintln!(
                "Enter a number from 1 to {} or a network name.",
                networks.len()
            );
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Mainnet => "mainnet",
            Self::Nile => "nile",
            Self::Ethereum => "ethereum",
            Self::Sepolia => "sepolia",
            Self::Polygon => "polygon",
            Self::Solana => "solana",
            Self::SolanaDevnet => "solana-devnet",
        }
    }

    pub fn chain(self) -> Chain {
        match self {
            Self::Mainnet => Chain::Tron(tron::Network::Mainnet),
            Self::Nile => Chain::Tron(tron::Network::Nile),
            Self::Ethereum => Chain::Ethereum(ethereum::Network::Mainnet),
            Self::Sepolia => Chain::Ethereum(ethereum::Network::Sepolia),
            Self::Polygon => Chain::Ethereum(ethereum::Network::Polygon),
            Self::Solana => Chain::Solana(solana::Network::Mainnet),
            Self::SolanaDevnet => Chain::Solana(solana::Network::Devnet),
        }
    }

    pub fn bridge_chain(self) -> Result<crate::bridge::Chain> {
        match self {
            Self::Mainnet => Ok(crate::bridge::Chain::Tron),
            Self::Ethereum => Ok(crate::bridge::Chain::Ethereum),
            Self::Polygon => Ok(crate::bridge::Chain::Polygon),
            Self::Solana => Ok(crate::bridge::Chain::Solana),
            Self::Nile | Self::Sepolia | Self::SolanaDevnet => {
                bail!("MetaMask-routed transfers require mainnet networks")
            }
        }
    }

    pub fn address(self, wallet: &Wallet) -> Result<String> {
        match self.chain() {
            Chain::Ethereum(_) => wallet.ethereum_address(),
            Chain::Tron(_) => Ok(wallet.address.clone()),
            Chain::Solana(_) => wallet.solana_address(),
        }
    }
}
