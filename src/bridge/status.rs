use anyhow::{Context, Result, ensure};
use reqwest::StatusCode;
use serde::Deserialize;

use super::{API, Chain, Client, Status};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Transfer {
    source_chain_symbol: String,
    destination_chain_symbol: String,
    send: Transaction,
    receive: Option<Transaction>,
    #[serde(default)]
    refunded: bool,
    refund_tx_id: Option<String>,
    #[serde(default)]
    signatures_count: u64,
    #[serde(default)]
    signatures_needed: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Transaction {
    tx_id: String,
    confirmations: u64,
    confirmations_needed: u64,
    #[serde(default)]
    is_claimable: bool,
}

impl Client {
    /// The API uses Allbridge chain symbols, NOT EVM chain IDs. This same
    /// source-transaction endpoint tracks OFT and indexed NEAR Intents deposits.
    pub fn status(&self, source: Chain, txid: &str) -> Result<Status> {
        let hash = txid.strip_prefix("0x").unwrap_or(txid);
        ensure!(
            hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
            "source transaction hash must contain 64 hexadecimal digits"
        );
        let txid = if source == Chain::Tron {
            hash.to_ascii_lowercase()
        } else {
            format!("0x{}", hash.to_ascii_lowercase())
        };
        let path = format!("/chain/{}/{txid}", source.api_symbol());
        let response = self
            .http
            .get(format!("{API}{path}"))
            .send()
            .context("querying Allbridge destination progress")?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(Status {
                state: "not indexed".to_owned(),
                destination_tx: None,
                details: Some("Allbridge has not indexed this source transaction. Check the source receipt and retry bridge-status later; this is not proof of failure or delivery.".to_owned()),
            });
        }
        let transfer: Transfer = self.response(response, &path)?;
        ensure!(
            transfer.source_chain_symbol == source.api_symbol(),
            "Allbridge returned a different source network"
        );
        ensure!(
            transfer
                .send
                .tx_id
                .trim_start_matches("0x")
                .eq_ignore_ascii_case(hash),
            "Allbridge returned a different source transaction"
        );
        if transfer.refunded {
            return Ok(Status {
                state: "refunded".to_owned(),
                destination_tx: None,
                details: Some(match transfer.refund_tx_id {
                    Some(tx) => format!("Returned on the source chain: {tx}; refund may exclude fees"),
                    None => "Returned on the source chain; refund transaction is not indexed yet; refund may exclude fees".to_owned(),
                }),
            });
        }
        if let Some(receive) = transfer.receive {
            let confirmed = receive.confirmations >= receive.confirmations_needed;
            return Ok(Status {
                state: if confirmed {
                    "completed"
                } else {
                    "destination confirming"
                }
                .to_owned(),
                destination_tx: Some(receive.tx_id),
                details: Some(format!(
                    "{} destination confirmations: {}/{}",
                    transfer.destination_chain_symbol,
                    receive.confirmations,
                    receive.confirmations_needed
                )),
            });
        }
        let state = if transfer.send.confirmations < transfer.send.confirmations_needed {
            "source confirming"
        } else if transfer.send.is_claimable {
            "claim required"
        } else {
            "awaiting destination"
        };
        let mut details = format!(
            "{} source confirmations: {}/{}; relay signatures: {}/{}; destination {}",
            transfer.source_chain_symbol,
            transfer.send.confirmations,
            transfer.send.confirmations_needed,
            transfer.signatures_count,
            transfer.signatures_needed,
            transfer.destination_chain_symbol
        );
        if transfer.send.is_claimable {
            details.push_str("; open https://core.allbridge.io/ to claim this transfer");
        }
        Ok(Status {
            state: state.to_owned(),
            destination_tx: None,
            details: Some(details),
        })
    }
}
