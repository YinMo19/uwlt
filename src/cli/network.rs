use std::io::{self, Write};

use anyhow::{Result, bail};
use clap::ValueEnum;

use crate::{ethereum, wallet::Wallet};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum Network {
    Mainnet,
    Ethereum,
    Polygon,
    Solana,
}

pub(super) enum Chain {
    Tron,
    Ethereum(ethereum::Network),
    Solana,
}

impl Network {
    pub fn prompt() -> Result<Self> {
        let networks = Self::value_variants();
        eprintln!("Select network:");
        for (index, network) in networks.iter().enumerate() {
            let description = match network {
                Self::Mainnet => "TRON mainnet",
                Self::Ethereum => "Ethereum mainnet",
                Self::Polygon => "Polygon PoS",
                Self::Solana => "Solana mainnet",
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
            Self::Ethereum => "ethereum",
            Self::Polygon => "polygon",
            Self::Solana => "solana",
        }
    }

    pub fn chain(self) -> Chain {
        match self {
            Self::Mainnet => Chain::Tron,
            Self::Ethereum => Chain::Ethereum(ethereum::Network::Mainnet),
            Self::Polygon => Chain::Ethereum(ethereum::Network::Polygon),
            Self::Solana => Chain::Solana,
        }
    }

    pub fn bridge_chain(self) -> crate::bridge::Chain {
        match self {
            Self::Mainnet => crate::bridge::Chain::Tron,
            Self::Ethereum => crate::bridge::Chain::Ethereum,
            Self::Polygon => crate::bridge::Chain::Polygon,
            Self::Solana => crate::bridge::Chain::Solana,
        }
    }

    pub fn address(self, wallet: &Wallet) -> Result<String> {
        match self.chain() {
            Chain::Ethereum(_) => wallet.ethereum_address(),
            Chain::Tron => Ok(wallet.address.clone()),
            Chain::Solana => wallet.solana_address(),
        }
    }
}
