use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::{
    Asset, Client, Prepared,
    transaction::{address_hex, default_zero, same_hex, text},
};

#[derive(Debug)]
pub struct FeeRates {
    /// SUN per energy unit.
    pub energy_sun: u64,
    /// SUN per bandwidth byte when neither resource pool can pay in full.
    pub bandwidth_sun: u64,
    /// System-contract account activation charge, excluding bandwidth.
    pub activation_sun: u64,
    pub(super) creation_bandwidth_sun: u64,
    pub(super) creation_bandwidth_ratio: u64,
}

#[derive(Debug)]
pub struct FeeEstimate {
    pub energy: u64,
    pub bandwidth: u64,
    /// Estimated SUN burned after currently available resources, not unit rates.
    pub energy_sun: u64,
    pub bandwidth_sun: u64,
    pub activation_sun: u64,
    pub total_sun: u64,
}

impl Client {
    pub fn fees(&self) -> Result<FeeRates> {
        let response = self.post("wallet/getchainparameters", &json!({}))?;
        let parameters = response["chainParameter"]
            .as_array()
            .context("missing chain parameters")?;
        let rate = |name: &str| -> Result<u64> {
            let entry = parameters
                .iter()
                .find(|entry| entry["key"].as_str() == Some(name))
                .with_context(|| format!("missing chain parameter {name}"))?;
            default_zero(entry, "value")
        };
        Ok(FeeRates {
            energy_sun: rate("getEnergyFee")?,
            bandwidth_sun: rate("getTransactionFee")?,
            activation_sun: rate("getCreateNewAccountFeeInSystemContract")?,
            creation_bandwidth_sun: rate("getCreateAccountFee")?,
            creation_bandwidth_ratio: rate("getCreateNewAccountBandwidthRate")?,
        })
    }

    /// A current resource/burn estimate, not a guaranteed fee or a fee_limit.
    pub fn fee_for(
        &self,
        prepared: &Prepared,
        from: &str,
        to: &str,
        asset: Asset,
    ) -> Result<FeeEstimate> {
        let from = address_hex(from)?;
        let to = address_hex(to)?;
        let rates = self.fees()?;
        let resources = self.resources(&from)?;
        let bandwidth = signed_bandwidth(&prepared.transaction)?;
        let energy = prepared.energy.unwrap_or(0);
        let available_energy = available(&resources, "EnergyLimit", "EnergyUsed")?;
        let caller_energy = if asset == Asset::Usdt {
            self.caller_energy(&from, energy)?
        } else {
            energy
        };
        let energy_sun = caller_energy
            .saturating_sub(available_energy)
            .checked_mul(rates.energy_sun)
            .context("energy fee overflow")?;
        let creates_account = if asset == Asset::Trx {
            let account = self.post(
                "wallet/getaccount",
                &json!({"address": to, "visible": false}),
            )?;
            if account.as_object().is_some_and(serde_json::Map::is_empty) {
                true
            } else {
                same_hex(&account, "address", &to)?;
                false
            }
        } else {
            // A TRC-20 balance does not activate a native TRON account.
            false
        };
        let staked = available(&resources, "NetLimit", "NetUsed")?;
        let free = available(&resources, "freeNetLimit", "freeNetUsed")?;
        let (bandwidth_sun, activation_sun) = if creates_account {
            let required = bandwidth
                .checked_mul(rates.creation_bandwidth_ratio)
                .context("account creation bandwidth overflow")?;
            (
                if staked >= required {
                    0
                } else {
                    rates.creation_bandwidth_sun
                },
                rates.activation_sun,
            )
        } else {
            (
                bandwidth_burn(bandwidth, staked, free, rates.bandwidth_sun)?,
                0,
            )
        };
        let total_sun = energy_sun
            .checked_add(bandwidth_sun)
            .and_then(|sum| sum.checked_add(activation_sun))
            .context("total fee overflow")?;
        Ok(FeeEstimate {
            energy,
            bandwidth,
            energy_sun,
            bandwidth_sun,
            activation_sun,
            total_sun,
        })
    }

    fn caller_energy(&self, from: &str, energy: u64) -> Result<u64> {
        let contract = self.post(
            "wallet/getcontract",
            &json!({
                "value": self.token, "visible": false
            }),
        )?;
        same_hex(&contract, "contract_address", &self.token)?;
        let percent = default_zero(&contract, "consume_user_resource_percent")?;
        ensure!(percent <= 100, "invalid contract energy sharing percentage");
        let origin = text(&contract, "origin_address")?;
        if percent == 100 || origin.eq_ignore_ascii_case(from) {
            return Ok(energy);
        }
        let resources = self.resources(origin)?;
        let origin_available = available(&resources, "EnergyLimit", "EnergyUsed")?;
        let origin_limit = default_zero(&contract, "origin_energy_limit")?;
        let share = (u128::from(energy) * u128::from(100 - percent) / 100) as u64;
        Ok(energy - share.min(origin_available).min(origin_limit))
    }

    pub(super) fn resources(&self, address: &str) -> Result<Value> {
        self.post(
            "wallet/getaccountresource",
            &json!({"address": address, "visible": false}),
        )
    }
}

pub(super) fn available(resources: &Value, limit: &str, used: &str) -> Result<u64> {
    Ok(default_zero(resources, limit)?.saturating_sub(default_zero(resources, used)?))
}

fn bandwidth_burn(bytes: u64, staked: u64, free: u64, price: u64) -> Result<u64> {
    // java-tron does not combine or partially spend the staked/free pools.
    // Free bandwidth also depends on the global free pool at inclusion time.
    if staked >= bytes || free >= bytes {
        Ok(0)
    } else {
        bytes.checked_mul(price).context("bandwidth fee overflow")
    }
}

pub(super) fn signed_bandwidth(transaction: &Value) -> Result<u64> {
    let raw = text(transaction, "raw_data_hex")?;
    let size = u64::try_from(raw.len() / 2)?;
    // Transaction.raw_data tag+length, one 65-byte signature tag+length, and
    // java-tron's MAX_RESULT_SIZE_IN_TX (64), not just raw_data bytes.
    let length_size = if size == 0 {
        1
    } else {
        u64::from((64 - size.leading_zeros()).div_ceil(7))
    };
    Ok(1 + length_size + size + 67 + 64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bandwidth_pools_are_not_combined_or_partially_burned() {
        assert_eq!(bandwidth_burn(300, 200, 200, 1000).unwrap(), 300_000);
        assert_eq!(bandwidth_burn(300, 300, 0, 1000).unwrap(), 0);
        assert_eq!(bandwidth_burn(300, 0, 300, 1000).unwrap(), 0);
    }

    #[test]
    fn protobuf_bandwidth_includes_signature_and_result_reservation() {
        let transaction = json!({"raw_data_hex": "00".repeat(128)});
        assert_eq!(signed_bandwidth(&transaction).unwrap(), 262);
    }
}
