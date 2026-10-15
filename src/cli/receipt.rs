use std::fmt::Display;

use alloy::primitives::U256;
use anyhow::{Result, bail};

use super::network::{Chain, Network};
use crate::{ethereum, transaction::Transfer, tron};

pub(super) fn native_amount(network: Network, units: U256) -> String {
    let (decimals, symbol) = match network.chain() {
        Chain::Tron(_) => (6, "TRX"),
        Chain::Ethereum(ethereum::Network::Polygon) => (18, "POL"),
        Chain::Ethereum(_) => (18, "ETH"),
    };
    format!("{} {symbol}", ethereum::format_amount(units, decimals))
}

pub(super) fn transaction(stage: &str, txid: &str) {
    println!("{stage}  {txid}");
}

pub(super) struct Fees {
    network: Network,
    total: U256,
    count: usize,
}

impl Fees {
    pub fn new(network: Network) -> Self {
        Self {
            network,
            total: U256::ZERO,
            count: 0,
        }
    }

    pub fn record(&mut self, stage: &str, fee: U256) {
        println!("Actual fee ({stage})  {}", native_amount(self.network, fee));
        self.total += fee;
        self.count += 1;
    }

    pub fn finish<T>(self, result: Result<T>) -> Result<T> {
        if self.count > 1 {
            let label = if result.is_ok() {
                "Actual fee total"
            } else {
                "Known fee total"
            };
            println!("{label}  {}", native_amount(self.network, self.total));
        }
        result
    }
}

fn field(label: &str, value: impl Display) {
    println!("{label:<13}{value}");
}

fn parties(from: Option<&str>, target: Option<&str>, transfer: Option<&Transfer>) {
    if let Some(from) = from {
        field("From", from);
    }
    if let Some(transfer) = transfer {
        field("To", &transfer.recipient);
        field(
            "Amount",
            format_args!("{} {}", transfer.amount, transfer.symbol),
        );
        if let Some(target) = target.filter(|target| *target != transfer.recipient) {
            field("Contract", target);
        }
    } else if let Some(target) = target {
        field("Tx target", target);
    }
}

pub(super) fn ethereum(
    network: Network,
    txid: &str,
    status: &ethereum::TxStatus,
    intent: Option<(&str, &Transfer)>,
) -> Result<()> {
    field("Network", network.label());
    field("Transaction", txid);
    field("State", format_args!("{:?}", status.state));
    if intent.is_none() {
        field("Finalized", status.finalized);
    }
    let transfer = intent
        .map(|(_, transfer)| transfer)
        .or(status.transfer.as_ref());
    parties(
        intent.map(|(from, _)| from).or(status.from.as_deref()),
        status.to.as_deref(),
        transfer,
    );
    if transfer.is_none() {
        if let Some(value) = &status.value {
            let symbol = if matches!(network.chain(), Chain::Ethereum(ethereum::Network::Polygon)) {
                "POL"
            } else {
                "ETH"
            };
            field("Tx value", format_args!("{value} {symbol}"));
        }
    }
    if let Some(fee) = status.actual_fee {
        field("Actual fee", native_amount(network, fee));
    }
    if let Some(gas) = status.gas_used {
        field("Gas used", gas);
    }
    if let Some(price) = status.gas_price {
        field(
            "Gas price",
            format_args!("{} Gwei", ethereum::format_amount(U256::from(price), 9)),
        );
    }
    if let Some(block) = status.block_number {
        field("Block", block);
    }
    if matches!(status.state, ethereum::TxState::Failed) {
        bail!("transaction reverted on chain");
    }
    Ok(())
}

pub(super) fn tron(
    network: Network,
    txid: &str,
    status: &tron::TxStatus,
    intent: Option<(&str, &Transfer)>,
) -> Result<()> {
    field("Network", network.label());
    field("Transaction", txid);
    field("State", format_args!("{:?}", status.state));
    field("Solidified", status.solidified);
    parties(
        intent.map(|(from, _)| from).or(status.from.as_deref()),
        status.to.as_deref(),
        intent
            .map(|(_, transfer)| transfer)
            .or(status.transfer.as_ref()),
    );
    if let Some(fee) = status.actual_fee {
        field("Actual fee", native_amount(network, fee));
    }
    if let Some(energy) = status.energy_used {
        field("Energy", energy);
    }
    if let Some(bandwidth) = status.bandwidth_used {
        field(
            "Bandwidth",
            format_args!("{bandwidth} points from resources"),
        );
    }
    if let Some(block) = status.block_number {
        field("Block", block);
    }
    if let Some(message) = &status.message {
        println!("{message}");
    }
    if matches!(status.state, tron::TxState::Failed) {
        bail!("transaction failed on chain");
    }
    Ok(())
}
