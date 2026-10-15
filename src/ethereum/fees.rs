use alloy::eips::BlockNumberOrTag;

use super::*;

/// Current RPC fee suggestions; the transfer reference is a cap, not an exact charge.
pub struct FeeRates {
    pub base_gwei: String,
    pub priority_gwei: String,
    pub max_gwei: String,
    pub native_transfer: String,
}

pub(super) struct FeeSnapshot {
    pub base: u128,
    pub priority: u128,
    pub maximum: u128,
}

impl Client {
    pub(super) async fn current_fees(&self) -> Result<FeeSnapshot> {
        let block = self
            .provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await?
            .context("latest EVM block is unavailable")?;
        let base = u128::from(
            block
                .header
                .base_fee_per_gas
                .context("network has no EIP-1559 base fee")?,
        );
        // Ask the node directly: Polygon's minimum tip need not match an Ethereum
        // fee-history estimator's defaults.
        let priority = self.provider.get_max_priority_fee_per_gas().await?;
        let maximum = base
            .checked_mul(2)
            .and_then(|fee| fee.checked_add(priority))
            .context("EIP-1559 fee exceeds uint128 range")?;
        Ok(FeeSnapshot {
            base,
            priority,
            maximum,
        })
    }

    pub fn fees(&self) -> Result<FeeRates> {
        self.run(async {
            let fees = self.current_fees().await?;
            Ok(FeeRates {
                base_gwei: amount::format(U256::from(fees.base), 9),
                priority_gwei: amount::format(U256::from(fees.priority), 9),
                max_gwei: amount::format(U256::from(fees.maximum), 9),
                native_transfer: amount::format(U256::from(21_000) * U256::from(fees.maximum), 18),
            })
        })
    }
}
