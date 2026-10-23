use super::*;
use std::{thread, time::Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxState {
    Pending,
    Confirmed,
    Failed,
    NotFound,
}
pub struct TxStatus {
    pub state: TxState,
    pub finalized: bool,
    pub slot: Option<u64>,
    /// Actual charged fee in lamports, including failed transactions.
    pub actual_fee: Option<u64>,
    pub error: Option<String>,
}
impl Client {
    pub fn status(&self, txid: &str) -> Result<TxStatus> {
        Signature::from_str(txid).context("invalid Solana transaction signature")?;
        let response = self.rpc(
            "getSignatureStatuses",
            json!([[txid], {"searchTransactionHistory":true}]),
        )?;
        let value = response["value"]
            .as_array()
            .and_then(|values| values.first())
            .context("invalid signature status response")?;
        let mut status = TxStatus {
            state: TxState::NotFound,
            finalized: false,
            slot: None,
            actual_fee: None,
            error: None,
        };
        if value.is_null() {
            return Ok(status);
        }
        status.slot = Some(
            value["slot"]
                .as_u64()
                .context("signature status has no slot")?,
        );
        let confirmation = value["confirmationStatus"]
            .as_str()
            .context("signature status lacks confirmation level")?;
        status.finalized = confirmation == "finalized";
        ensure!(
            ["processed", "confirmed", "finalized"].contains(&confirmation),
            "unknown signature confirmation level"
        );
        ensure!(
            value.get("err").is_some(),
            "signature status lacks error field"
        );
        let confirmed = confirmation != "processed";
        status.state = if !confirmed {
            TxState::Pending
        } else if value["err"].is_null() {
            TxState::Confirmed
        } else {
            TxState::Failed
        };
        if !value["err"].is_null() {
            status.error = Some(value["err"].to_string());
        }
        if confirmed {
            let receipt = self.rpc("getTransaction", json!([txid, {"encoding":"json", "commitment":"confirmed", "maxSupportedTransactionVersion":1}]))?;
            if !receipt.is_null() {
                ensure!(
                    receipt["transaction"]["signatures"][0].as_str() == Some(txid),
                    "RPC receipt belongs to a different transaction"
                );
                ensure!(
                    receipt["slot"].as_u64() == status.slot,
                    "RPC receipt slot disagrees with signature status"
                );
                let meta = receipt
                    .get("meta")
                    .filter(|meta| !meta.is_null())
                    .context("receipt has no transaction metadata")?;
                ensure!(
                    meta.get("err").is_some() && meta["err"] == value["err"],
                    "receipt execution result disagrees with signature status"
                );
                status.actual_fee = Some(meta["fee"].as_u64().context("receipt has no fee")?);
            }
        }
        Ok(status)
    }
    /// Status reads are polling, never transaction retries or replacement signing.
    pub fn wait_for_receipt(&self, txid: &str) -> Result<TxStatus> {
        let deadline = Instant::now() + Duration::from_secs(180);
        (|| {
            loop {
                let status = self.status(txid)?;
                if matches!(status.state, TxState::Confirmed | TxState::Failed) && status.actual_fee.is_some() { return Ok(status); }
                ensure!(Instant::now() < deadline, "receipt wait timed out after 180 seconds");
                thread::sleep(Duration::from_secs(2));
            }
        })().with_context(|| format!("receipt wait for {txid} failed; outcome is uncertain; inspect this signature before sending again"))
    }
}
