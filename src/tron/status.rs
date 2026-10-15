use std::{
    thread,
    time::{Duration, Instant},
};

use alloy::primitives::U256;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::transaction::Transfer;

use super::{
    Client, format_amount,
    transaction::{decode_message, default_zero, same_hex, text, unsigned, validate_txid},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxState {
    Pending,
    /// Included and executed successfully; consult solidified for finality.
    Confirmed,
    Failed,
    /// Absent from this node's chain and pending pool, not proof of global absence.
    NotFound,
}

#[derive(Debug)]
pub struct TxStatus {
    pub state: TxState,
    pub solidified: bool,
    pub block_number: Option<u64>,
    /// Total receipt fee in SUN; absent until an execution receipt is available.
    pub actual_fee: Option<U256>,
    pub from: Option<String>,
    /// Raw transaction destination, which is the token contract for TRC-20 calls.
    pub to: Option<String>,
    pub transfer: Option<Transfer>,
    pub energy_used: Option<u64>,
    pub bandwidth_used: Option<u64>,
    pub message: Option<String>,
}

impl Client {
    pub fn status(&self, txid: &str) -> Result<TxStatus> {
        validate_txid(txid)?;
        let query = json!({"value": txid, "visible": false});
        if let Some(mut status) = self.transaction_receipt(txid, &query)? {
            let transaction = self.post("wallet/gettransactionbyid", &query)?;
            if !empty(&transaction) {
                transaction_details(&mut status, &transaction, txid, &self.token)?;
            }
            return Ok(status);
        }
        let transaction = self.post("wallet/gettransactionbyid", &query)?;
        if !empty(&transaction) {
            let mut status = unmined(
                TxState::Pending,
                Some("transaction known to node; execution receipt not yet available".into()),
            );
            transaction_details(&mut status, &transaction, txid, &self.token)?;
            return Ok(status);
        }
        let pending = self.post("wallet/gettransactionfrompending", &query)?;
        if !empty(&pending) {
            let mut status = unmined(TxState::Pending, None);
            transaction_details(&mut status, &pending, txid, &self.token)?;
            return Ok(status);
        }
        Ok(unmined(
            TxState::NotFound,
            Some(
                "not found on this node; absence does not prove the transaction was never broadcast"
                    .into(),
            ),
        ))
    }

    fn transaction_receipt(&self, txid: &str, query: &Value) -> Result<Option<TxStatus>> {
        let solid = self.post("walletsolidity/gettransactioninfobyid", query)?;
        if !empty(&solid) {
            return receipt(&solid, txid, true).map(Some);
        }
        let current = self.post("wallet/gettransactioninfobyid", query)?;
        if !empty(&current) {
            return receipt(&current, txid, false).map(Some);
        }
        Ok(None)
    }

    pub fn wait_for_receipt(&self, txid: &str) -> Result<TxStatus> {
        validate_txid(txid).with_context(|| format!("invalid transaction hash {txid}"))?;
        let query = json!({"value": txid, "visible": false});
        let started = Instant::now();
        let timeout = Duration::from_secs(180);
        loop {
            ensure!(
                started.elapsed() < timeout,
                "timed out awaiting receipt for {txid}; outcome is unknown; query status, do not rebroadcast"
            );
            let status = self.transaction_receipt(txid, &query).with_context(|| {
                format!("receipt unknown for {txid}; query status, do not rebroadcast")
            })?;
            // Inclusion is sufficient here; solidified separately describes finality.
            if let Some(status) = status {
                return Ok(status);
            }
            thread::sleep(Duration::from_secs(3).min(timeout.saturating_sub(started.elapsed())));
        }
    }
}

fn empty(value: &Value) -> bool {
    value.as_object().is_some_and(serde_json::Map::is_empty)
}

fn unmined(state: TxState, message: Option<String>) -> TxStatus {
    TxStatus {
        state,
        solidified: false,
        block_number: None,
        actual_fee: None,
        from: None,
        to: None,
        transfer: None,
        energy_used: None,
        bandwidth_used: None,
        message,
    }
}

fn transaction_details(
    status: &mut TxStatus,
    transaction: &Value,
    txid: &str,
    token: &str,
) -> Result<()> {
    same_hex(transaction, "txID", txid)?;
    let contracts = transaction
        .pointer("/raw_data/contract")
        .and_then(Value::as_array)
        .context("transaction missing contracts")?;
    let [contract] = contracts.as_slice() else {
        return Ok(());
    };
    let parameter = contract
        .pointer("/parameter/value")
        .context("transaction missing contract parameters")?;
    if parameter.get("owner_address").is_some() {
        status.from = Some(display_address(text(parameter, "owner_address")?)?);
    }
    let destination = if parameter.get("contract_address").is_some() {
        Some(text(parameter, "contract_address")?)
    } else if parameter.get("to_address").is_some() {
        Some(text(parameter, "to_address")?)
    } else {
        None
    };
    status.to = destination.map(display_address).transpose()?;
    match text(contract, "type")? {
        "TransferContract" => {
            status.transfer = Some(Transfer {
                recipient: display_address(text(parameter, "to_address")?)?,
                amount: format_amount(default_zero(parameter, "amount")?),
                symbol: "TRX",
            });
        }
        "TriggerSmartContract"
            if destination.is_some_and(|address| address.eq_ignore_ascii_case(token)) =>
        {
            let Some(data) = parameter.get("data") else {
                return Ok(());
            };
            let data = hex::decode(data.as_str().context("invalid contract calldata")?)
                .context("invalid contract calldata hex")?;
            if data.len() == 68
                && data[..4] == [0xa9, 0x05, 0x9c, 0xbb]
                && data[4..16].iter().all(|byte| *byte == 0)
            {
                let mut address = [0_u8; 21];
                address[0] = 0x41;
                address[1..].copy_from_slice(&data[16..36]);
                let amount = crate::ethereum::format_amount(U256::from_be_slice(&data[36..]), 6);
                status.transfer = Some(Transfer {
                    recipient: bs58::encode(address).with_check().into_string(),
                    amount,
                    symbol: "USDT",
                });
            }
        }
        _ => {}
    }
    Ok(())
}

fn display_address(address: &str) -> Result<String> {
    let bytes = hex::decode(address).context("invalid TRON address hex")?;
    ensure!(
        bytes.len() == 21 && bytes[0] == 0x41,
        "invalid TRON address prefix or length"
    );
    Ok(bs58::encode(bytes).with_check().into_string())
}

fn receipt(value: &Value, txid: &str, solidified: bool) -> Result<TxStatus> {
    same_hex(value, "id", txid)?;
    let block_number = unsigned(value, "blockNumber")?;
    let resources = value
        .get("receipt")
        .context("transaction info missing execution receipt")?;
    ensure!(
        resources.is_object(),
        "invalid transaction execution receipt"
    );
    // Protobuf JSON omits the successful default TransactionInfo.result (SUCESS),
    // and native TRX transfers do not have a TVM receipt.result at all.
    let mut failure = None;
    if value.get("result").is_some() {
        let result = text(value, "result")?;
        if result != "SUCESS" && result != "SUCCESS" {
            failure = Some(result.to_owned());
        }
    }
    if resources.get("result").is_some() {
        let result = text(resources, "result")?;
        if result != "SUCCESS" {
            failure = Some(result.to_owned());
        }
    }
    let state = if failure.is_some() {
        TxState::Failed
    } else {
        TxState::Confirmed
    };
    let message = match value.get("resMessage") {
        Some(message) => {
            let message = message
                .as_str()
                .context("invalid transaction result message")?;
            let decoded = decode_message(message);
            if decoded.is_empty() {
                failure
            } else {
                Some(decoded)
            }
        }
        None => failure,
    };
    Ok(TxStatus {
        state,
        solidified,
        block_number: Some(block_number),
        actual_fee: Some(U256::from(default_zero(value, "fee")?)),
        from: None,
        to: None,
        transfer: None,
        energy_used: Some(default_zero(resources, "energy_usage_total")?),
        bandwidth_used: Some(default_zero(resources, "net_usage")?),
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_distinguishes_failure_and_finality() {
        let txid = "ab".repeat(32);
        let trx = json!({"id": txid, "blockNumber": 123, "receipt": {"net_usage": 267}});
        let status = receipt(&trx, &txid, false).unwrap();
        assert_eq!(status.state, TxState::Confirmed);
        assert!(!status.solidified);
        assert_eq!(status.actual_fee, Some(U256::ZERO));
        let reverted = json!({"id": txid, "blockNumber": 123, "fee": 1_500_000,
            "receipt": {"result": "REVERT", "energy_fee": 1_000_000, "net_fee": 100_000,
                "energy_usage_total": 25_000},
            "result": "FAILED", "resMessage": hex::encode("execution reverted")});
        let status = receipt(&reverted, &txid, true).unwrap();
        assert_eq!(status.state, TxState::Failed);
        assert!(status.solidified);
        assert_eq!(status.actual_fee, Some(U256::from(1_500_000)));
        assert_eq!(status.message.as_deref(), Some("execution reverted"));
        assert!(receipt(&json!({"id": txid}), &txid, false).is_err());
        assert!(receipt(&trx, &"cd".repeat(32), false).is_err());
    }

    #[test]
    fn details_distinguish_transfer_recipient_from_contract_destination() {
        let txid = "ab".repeat(32);
        let owner = "410000000000000000000000000000000000000001";
        let recipient = "410000000000000000000000000000000000000002";
        let token = "410000000000000000000000000000000000000003";
        let mut transaction = json!({
            "txID": txid,
            "raw_data": {"contract": [{
                "type": "TransferContract",
                "parameter": {"value": {
                    "owner_address": owner, "to_address": recipient, "amount": 1_230_000
                }}
            }]}
        });
        let mut status = unmined(TxState::Pending, None);
        transaction_details(&mut status, &transaction, &txid, token).unwrap();
        assert_eq!(status.from, Some(display_address(owner).unwrap()));
        assert_eq!(status.to, Some(display_address(recipient).unwrap()));
        let transfer = status.transfer.as_ref().unwrap();
        assert_eq!(transfer.recipient, display_address(recipient).unwrap());
        assert_eq!(transfer.amount, "1.23");
        assert_eq!(transfer.symbol, "TRX");

        transaction["raw_data"]["contract"][0] = json!({
            "type": "TriggerSmartContract",
            "parameter": {"value": {
                "owner_address": owner,
                "contract_address": token,
                "data": format!("a9059cbb{}", super::super::transaction::transfer_parameter(recipient, 1))
            }}
        });
        let mut status = unmined(TxState::Pending, None);
        transaction_details(&mut status, &transaction, &txid, token).unwrap();
        assert_eq!(status.to, Some(display_address(token).unwrap()));
        let transfer = status.transfer.as_ref().unwrap();
        assert_eq!(transfer.recipient, display_address(recipient).unwrap());
        assert_eq!(transfer.amount, "0.000001");
        assert_eq!(transfer.symbol, "USDT");

        let mut status = unmined(TxState::Pending, None);
        transaction_details(&mut status, &transaction, &txid, owner).unwrap();
        assert_eq!(status.to, Some(display_address(token).unwrap()));
        assert!(status.transfer.is_none());
        transaction["raw_data"]["contract"][0]["parameter"]["value"]["data"] = json!("095ea7b3");
        let mut status = unmined(TxState::Pending, None);
        transaction_details(&mut status, &transaction, &txid, token).unwrap();
        assert!(status.transfer.is_none());
    }
}
