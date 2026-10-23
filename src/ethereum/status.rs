use std::time::Duration;

use alloy::{
    consensus::Transaction as _,
    eips::BlockNumberOrTag,
    primitives::{Address, B256, U256},
    providers::Provider,
    rpc::types::TransactionReceipt,
    sol_types::SolCall,
};
use anyhow::{Context, Result, ensure};

use super::{Client, amount, transferCall};

const RECEIPT_TIMEOUT: Duration = Duration::from_secs(180);

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
    pub block_number: Option<u64>,
    /// Receipt-derived charge in wei; absent until a receipt is available.
    pub actual_fee: Option<U256>,
    pub from: Option<String>,
    /// Raw transaction destination, not the recipient encoded in token calldata.
    pub to: Option<String>,
    pub transfer: Option<crate::transaction::Transfer>,
    pub value: Option<String>,
    pub gas_used: Option<u64>,
    pub gas_price: Option<u128>,
}

impl Client {
    pub fn status(&self, txid: &str) -> Result<TxStatus> {
        let hash: B256 = txid.parse().context("invalid EVM transaction hash")?;
        self.run(async {
            let mut status =
                if let Some(receipt) = self.provider.get_transaction_receipt(hash).await? {
                    let mut status = self.canonical_receipt_status(&receipt).await?;
                    let finalized = self
                        .provider
                        .get_block_by_number(BlockNumberOrTag::Finalized)
                        .await?
                        .context("finalized block is unavailable")?;
                    status.finalized = receipt
                        .block_number
                        .is_some_and(|number| number <= finalized.header.number);
                    status
                } else {
                    TxStatus {
                        state: TxState::NotFound,
                        finalized: false,
                        block_number: None,
                        actual_fee: None,
                        from: None,
                        to: None,
                        transfer: None,
                        value: None,
                        gas_used: None,
                        gas_price: None,
                    }
                };
            if let Some(transaction) = self.provider.get_transaction_by_hash(hash).await? {
                if status.actual_fee.is_none() {
                    status.state = TxState::Pending;
                    status.block_number = transaction.block_number;
                }
                status.from = Some(transaction.inner.signer().to_checksum(None));
                status.to = transaction.to().map(|address| address.to_checksum(None));
                status.value = Some(amount::format(transaction.value(), 18));
                status.transfer = self.decode_transfer(
                    transaction.to(),
                    transaction.value(),
                    transaction.input(),
                );
            }
            Ok(status)
        })
    }

    /// Wait for canonical inclusion, returning failed receipts with their charged fee.
    /// Finality is queried separately by `status`; inclusion alone is not finality.
    pub fn wait_for_receipt(&self, txid: &str) -> Result<TxStatus> {
        let hash: B256 = txid.parse().context("invalid EVM transaction hash")?;
        self.runtime.block_on(async {
            tokio::time::timeout(RECEIPT_TIMEOUT, async {
                loop {
                    if let Some(receipt) = self.provider.get_transaction_receipt(hash).await? {
                        return self.canonical_receipt_status(&receipt).await;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            })
            .await
            .context("receipt wait timed out after 180 seconds")?
        })
        .with_context(|| {
            format!(
                "receipt wait for {hash} failed; outcome is uncertain; inspect this hash before sending again"
            )
        })
    }

    async fn canonical_receipt_status(&self, receipt: &TransactionReceipt) -> Result<TxStatus> {
        let number = receipt
            .block_number
            .context("receipt has no block number")?;
        let block_hash = receipt.block_hash.context("receipt has no block hash")?;
        let canonical = self
            .provider
            .get_block_by_number(number.into())
            .await?
            .context("receipt block is unavailable")?;
        ensure!(
            canonical.header.hash == block_hash,
            "receipt was reorganized; query status again"
        );
        Ok(mined_status(receipt))
    }

    fn decode_transfer(
        &self,
        destination: Option<Address>,
        value: U256,
        input: &[u8],
    ) -> Option<crate::transaction::Transfer> {
        let destination = destination?;
        if input.is_empty() {
            return Some(crate::transaction::Transfer {
                recipient: destination.to_checksum(None),
                amount: amount::format(value, 18),
                symbol: self.native_symbol(),
            });
        }
        if destination == self.token {
            let transfer = transferCall::abi_decode(input).ok()?;
            return Some(crate::transaction::Transfer {
                recipient: transfer.recipient.to_checksum(None),
                amount: amount::format(transfer.value, 6),
                symbol: self.token_symbol(),
            });
        }
        None
    }
}

fn mined_status(receipt: &TransactionReceipt) -> TxStatus {
    let mut fee = U256::from(receipt.gas_used) * U256::from(receipt.effective_gas_price);
    if let (Some(used), Some(price)) = (receipt.blob_gas_used, receipt.blob_gas_price) {
        fee += U256::from(used) * U256::from(price);
    }
    TxStatus {
        state: if receipt.status() {
            TxState::Confirmed
        } else {
            TxState::Failed
        },
        finalized: false,
        block_number: receipt.block_number,
        actual_fee: Some(fee),
        from: Some(receipt.from.to_checksum(None)),
        to: receipt.to.map(|address| address.to_checksum(None)),
        transfer: None,
        value: None,
        gas_used: Some(receipt.gas_used),
        gas_price: Some(receipt.effective_gas_price),
    }
}

#[cfg(test)]
mod tests {
    use super::super::POLYGON_USDT;
    use super::*;
    use alloy::providers::ProviderBuilder;
    use tokio::runtime::Builder;

    fn receipt(success: bool) -> TransactionReceipt {
        serde_json::from_value(serde_json::json!({
            "type": "0x2",
            "status": if success { "0x1" } else { "0x0" },
            "cumulativeGasUsed": "0x5208",
            "logs": [],
            "logsBloom": format!("0x{}", "00".repeat(256)),
            "transactionHash": B256::repeat_byte(1),
            "transactionIndex": "0x0",
            "blockHash": B256::repeat_byte(2),
            "blockNumber": "0xa",
            "gasUsed": "0x5208",
            "effectiveGasPrice": "0x3b9aca00",
            "from": Address::repeat_byte(3),
            "to": Address::repeat_byte(4)
        }))
        .unwrap()
    }

    #[test]
    fn mined_success_and_failure_report_actual_fees() {
        for success in [true, false] {
            let receipt = receipt(success);
            let status = mined_status(&receipt);
            assert_eq!(
                status.state,
                if success {
                    TxState::Confirmed
                } else {
                    TxState::Failed
                }
            );
            assert_eq!(status.actual_fee, Some(U256::from(21_000_000_000_000u64)));
        }
    }

    #[test]
    fn receipt_fee_includes_blob_charge_and_uses_full_width_arithmetic() {
        let mut receipt = receipt(true);
        receipt.gas_used = 30_000_000;
        receipt.effective_gas_price = 1_000_000_000_000;
        receipt.blob_gas_used = Some(131_072);
        receipt.blob_gas_price = Some(1_000_000_000);
        assert_eq!(
            mined_status(&receipt).actual_fee,
            Some(U256::from(30_000_131_072_000_000_000_u128))
        );
        receipt.effective_gas_price = 0;
        receipt.blob_gas_used = None;
        receipt.blob_gas_price = None;
        assert_eq!(mined_status(&receipt).actual_fee, Some(U256::ZERO));
    }

    #[test]
    fn transfer_details_distinguish_recipient_from_contract_destination() {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        let provider = {
            let _guard = runtime.enter();
            ProviderBuilder::default().connect_http("http://127.0.0.1:1".parse().unwrap())
        };
        let client = Client {
            provider,
            runtime,
            chain_id: 137,
            token: POLYGON_USDT,
        };
        let recipient = Address::repeat_byte(5);
        let input = transferCall {
            recipient,
            value: U256::from(1_234_567),
        }
        .abi_encode();
        let token = client
            .decode_transfer(Some(POLYGON_USDT), U256::ZERO, &input)
            .unwrap();
        assert_eq!(token.recipient, recipient.to_checksum(None));
        assert_eq!(token.amount, "1.234567");
        assert_eq!(token.symbol, "USDT0");
        assert!(
            client
                .decode_transfer(Some(recipient), U256::ZERO, &input)
                .is_none()
        );
        assert!(
            client
                .decode_transfer(Some(POLYGON_USDT), U256::ZERO, &[1])
                .is_none()
        );
        let native = client
            .decode_transfer(Some(recipient), U256::from(1), &[])
            .unwrap();
        assert_eq!(native.recipient, recipient.to_checksum(None));
        assert_eq!(native.amount, "0.000000000000000001");
        assert_eq!(native.symbol, "POL");
        assert!(client.decode_transfer(None, U256::from(1), &[]).is_none());
    }
}
