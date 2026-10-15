use anyhow::{Result, bail};
use clap::ValueEnum;

use crate::{ethereum, tron, wallet::Wallet};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum Network {
    Mainnet,
    Nile,
    Ethereum,
    Sepolia,
    Polygon,
}

pub(super) enum Chain {
    Tron(tron::Network),
    Ethereum(ethereum::Network),
}

impl Network {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mainnet => "mainnet",
            Self::Nile => "nile",
            Self::Ethereum => "ethereum",
            Self::Sepolia => "sepolia",
            Self::Polygon => "polygon",
        }
    }

    pub fn chain(self) -> Chain {
        match self {
            Self::Mainnet => Chain::Tron(tron::Network::Mainnet),
            Self::Nile => Chain::Tron(tron::Network::Nile),
            Self::Ethereum => Chain::Ethereum(ethereum::Network::Mainnet),
            Self::Sepolia => Chain::Ethereum(ethereum::Network::Sepolia),
            Self::Polygon => Chain::Ethereum(ethereum::Network::Polygon),
        }
    }

    pub fn bridge_chain(self) -> Result<crate::bridge::Chain> {
        match self {
            Self::Mainnet => Ok(crate::bridge::Chain::Tron),
            Self::Ethereum => Ok(crate::bridge::Chain::Ethereum),
            Self::Polygon => Ok(crate::bridge::Chain::Polygon),
            Self::Nile | Self::Sepolia => bail!("Allbridge transfers require mainnet networks"),
        }
    }

    pub fn address(self, wallet: &Wallet) -> Result<String> {
        match self.chain() {
            Chain::Ethereum(_) => wallet.ethereum_address(),
            Chain::Tron(_) => Ok(wallet.address.clone()),
        }
    }
}
