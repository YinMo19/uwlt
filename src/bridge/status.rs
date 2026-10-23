use alloy::primitives::U256;
use anyhow::{Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;

use super::{Chain, Client, Status, amount, wallet_address};

// MetaMask core: bridge-status-controller/src/utils/{bridge-status,validators}.ts.
// COMPLETE is a provider report, not independent proof of recipient delivery.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusResponse {
    status: String,
    src_chain: ChainStatus,
    dest_chain: Option<ChainStatus>,
    is_expected_token: Option<bool>,
    is_unrecognized_router_address: Option<bool>,
    error: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChainStatus {
    chain_id: u64,
    tx_hash: Option<String>,
    amount: Option<String>,
    token: Option<StatusToken>,
}

// The official schema explicitly allows an empty token object.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusToken {
    chain_id: Option<u64>,
    address: Option<String>,
    decimals: Option<u8>,
}

impl Client {
    /// Track the bridge request, not its approval or just the source receipt.
    pub fn status(
        &self,
        source: Chain,
        destination: Chain,
        bridge_id: &str,
        request_id: &str,
        txid: &str,
    ) -> Result<Status> {
        ensure!(
            source != destination,
            "bridge tracking requires different networks"
        );
        for (name, value) in [("bridge ID", bridge_id), ("request ID", request_id)] {
            ensure!(
                !value.trim().is_empty() && !value.chars().any(char::is_control),
                "MetaMask {name} must be nonempty and contain no control characters"
            );
        }
        let txid = transaction_hash(source, txid)?;
        let response: StatusResponse = self.get(
            "/getTxStatus",
            &[
                ("bridgeId", bridge_id),
                ("requestId", request_id),
                ("srcChainId", source.chain_id()),
                ("destChainId", destination.chain_id()),
                ("srcTxHash", &txid),
                ("refuel", "false"),
            ],
        )?;
        response.into_status(source, destination, &txid)
    }
}

fn transaction_hash(chain: Chain, txid: &str) -> Result<String> {
    if chain == Chain::Solana {
        let mut signature = [0; 64];
        let length = bs58::decode(txid)
            .onto(&mut signature)
            .map_err(|_| anyhow::anyhow!("Solana transaction signature must be base58"))?;
        ensure!(
            length == 64 && signature != [0; 64],
            "Solana transaction signature must encode 64 bytes and must not be all zero"
        );
        return Ok(txid.to_owned());
    }
    let hash = txid.strip_prefix("0x").unwrap_or(txid);
    ensure!(
        hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{} transaction hash must contain 64 hexadecimal digits",
        chain.label()
    );
    Ok(if chain == Chain::Tron {
        hash.to_ascii_lowercase()
    } else {
        format!("0x{}", hash.to_ascii_lowercase())
    })
}

fn chain_id(chain: Chain) -> u64 {
    match chain {
        Chain::Ethereum => 1,
        Chain::Polygon => 137,
        Chain::Tron => 728_126_428,
        Chain::Solana => 1_151_111_081_099_710,
    }
}

impl StatusResponse {
    fn into_status(self, source: Chain, destination: Chain, txid: &str) -> Result<Status> {
        ensure!(
            self.error.is_none(),
            "MetaMask returned a bridge status error; delivery is unknown"
        );
        ensure!(
            self.src_chain.chain_id == chain_id(source),
            "MetaMask returned a different source network"
        );
        let source_hash = self
            .src_chain
            .tx_hash
            .as_deref()
            .map(|hash| transaction_hash(source, hash))
            .transpose()?;
        if let Some(hash) = &source_hash {
            ensure!(
                *hash == transaction_hash(source, txid)?,
                "MetaMask returned a different source transaction"
            );
        }
        let mut destination_hash = None;
        let mut output_verified = false;
        if let Some(output) = &self.dest_chain {
            ensure!(
                output.chain_id == chain_id(destination),
                "MetaMask returned a different destination network; delivery or refund is unverified"
            );
            destination_hash = output
                .tx_hash
                .as_deref()
                .map(|hash| transaction_hash(destination, hash))
                .transpose()?;
            let units = output.amount.as_deref().map(amount::integer).transpose()?;
            if let Some(token) = &output.token {
                output_verified = token.chain_id == Some(chain_id(destination))
                    && token.decimals == Some(6)
                    && token.address.as_deref().is_some_and(|token_address| {
                        wallet_address(destination, token_address).ok()
                            == wallet_address(destination, destination.token()).ok()
                    })
                    && units.is_some_and(|units| units > U256::ZERO);
            }
        }
        let (state, detail) = match self.status.as_str() {
            "SUBMITTED" => (
                "submitted",
                "Source transaction submitted; destination delivery is not confirmed.",
            ),
            "PENDING" => (
                "pending",
                "Still pending; source confirmation alone does not prove destination delivery.",
            ),
            "UNKNOWN" => (
                "unknown",
                "MetaMask cannot determine the transfer status; neither delivery nor refund is confirmed.",
            ),
            "FAILED" => (
                "failed",
                "MetaMask reports failure; neither destination delivery nor a refund is confirmed. Check the source and destination before retrying.",
            ),
            "COMPLETE"
                if source_hash.is_some()
                    && destination_hash.is_some()
                    && output_verified
                    && self.is_expected_token != Some(false)
                    && self.is_unrecognized_router_address != Some(true) =>
            {
                (
                    "completed",
                    "MetaMask reports a positive amount of the configured USDT/USDT0 on the destination chain. This provider report is not independent verification of recipient delivery.",
                )
            }
            "COMPLETE" => (
                "output unverified",
                "MetaMask reports COMPLETE but matching source/destination transactions and the expected token output are not all verified; do not assume delivery or a refund.",
            ),
            other => {
                bail!("unknown MetaMask transaction status: {other:?}; delivery is unverified")
            }
        };
        let mut details = detail.to_owned();
        if state != "completed" {
            if let Some(hash) = destination_hash.take() {
                details.push_str(&format!(
                    " Destination transaction: {hash} (not proof of delivery)."
                ));
            }
        }
        Ok(Status {
            state: state.to_owned(),
            destination_tx: destination_hash,
            details: Some(details),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(status: &str) -> Value {
        json!({
            "status": status,
            "srcChain": {"chainId": 1, "txHash": format!("0x{}", "ab".repeat(32))},
            "destChain": {
                "chainId": 137,
                "txHash": format!("0x{}", "cd".repeat(32)),
                "amount": "99000000",
                "token": {"chainId": 137, "address": Chain::Polygon.token(), "decimals": 6}
            },
            "isExpectedToken": true
        })
    }

    fn interpret(value: Value) -> Result<Status> {
        serde_json::from_value::<StatusResponse>(value)?.into_status(
            Chain::Ethereum,
            Chain::Polygon,
            &format!("0x{}", "ab".repeat(32)),
        )
    }

    #[test]
    fn nonterminal_and_failed_states_never_claim_delivery_or_refund() {
        for (phase, expected) in [
            ("SUBMITTED", "submitted"),
            ("PENDING", "pending"),
            ("UNKNOWN", "unknown"),
            ("FAILED", "failed"),
        ] {
            let status = interpret(response(phase)).unwrap();
            assert_eq!(status.state, expected);
            assert!(status.destination_tx.is_none());
        }
    }

    #[test]
    fn complete_requires_source_and_destination_evidence() {
        let status = interpret(response("COMPLETE")).unwrap();
        assert_eq!(status.state, "completed");
        assert_eq!(
            status.destination_tx.unwrap(),
            format!("0x{}", "cd".repeat(32))
        );
        for (parent, field) in [
            ("srcChain", "txHash"),
            ("destChain", "txHash"),
            ("destChain", "amount"),
            ("destChain", "token"),
        ] {
            let mut value = response("COMPLETE");
            value[parent].as_object_mut().unwrap().remove(field);
            let status = interpret(value).unwrap();
            assert_eq!(status.state, "output unverified");
            assert!(status.destination_tx.is_none());
        }
        let mut value = response("COMPLETE");
        value.as_object_mut().unwrap().remove("destChain");
        assert_eq!(interpret(value).unwrap().state, "output unverified");
    }

    #[test]
    fn mismatched_chains_and_hashes_are_rejected_even_while_pending() {
        for phase in ["PENDING", "COMPLETE"] {
            for side in ["srcChain", "destChain"] {
                let mut value = response(phase);
                value[side]["chainId"] = json!(728126428);
                assert!(interpret(value).is_err());
                let mut value = response(phase);
                value[side]["txHash"] = json!("not-a-hash");
                assert!(interpret(value).is_err());
            }
            let mut value = response(phase);
            value["srcChain"]["txHash"] = json!(format!("0x{}", "ef".repeat(32)));
            assert!(interpret(value).is_err());
        }
    }

    #[test]
    fn wrong_empty_or_zero_output_is_not_delivery() {
        for token in [
            json!({}),
            json!({"chainId": 137, "address": Chain::Ethereum.token(), "decimals": 6}),
            json!({"chainId": 1, "address": Chain::Polygon.token(), "decimals": 6}),
            json!({"chainId": 137, "address": Chain::Polygon.token(), "decimals": 18}),
        ] {
            let mut value = response("COMPLETE");
            value["destChain"]["token"] = token;
            assert_eq!(interpret(value).unwrap().state, "output unverified");
        }
        let mut value = response("COMPLETE");
        value["destChain"]["amount"] = json!("0");
        assert_eq!(interpret(value).unwrap().state, "output unverified");
        for (flag, setting) in [
            ("isExpectedToken", false),
            ("isUnrecognizedRouterAddress", true),
        ] {
            let mut value = response("COMPLETE");
            value[flag] = json!(setting);
            assert_eq!(interpret(value).unwrap().state, "output unverified");
        }
    }

    #[test]
    fn unknown_states_and_error_payloads_are_not_success() {
        for phase in ["SUCCESS", "REFUNDED", "", "complete"] {
            assert!(interpret(response(phase)).is_err());
        }
        for error in [json!("upstream unavailable"), json!({"code": "UNKNOWN"})] {
            let mut value = response("COMPLETE");
            value["error"] = error;
            assert!(interpret(value).is_err());
        }
        assert!(interpret(json!({"message": "Invalid status response from aggregator"})).is_err());
        let mut value = response("COMPLETE");
        value["destChain"]["amount"] = json!("-1");
        assert!(interpret(value).is_err());
    }

    #[test]
    fn tron_and_evm_hashes_are_normalized_without_changing_identity() {
        let hash = "AB".repeat(32);
        assert_eq!(
            transaction_hash(Chain::Tron, &format!("0x{hash}")).unwrap(),
            hash.to_ascii_lowercase()
        );
        assert_eq!(
            transaction_hash(Chain::Ethereum, &hash).unwrap(),
            format!("0x{}", hash.to_ascii_lowercase())
        );
        let mut value = response("COMPLETE");
        value["srcChain"]["txHash"] = json!(hash);
        assert_eq!(interpret(value).unwrap().state, "completed");
        assert!(transaction_hash(Chain::Tron, "abc").is_err());
    }

    #[test]
    fn solana_signatures_keep_case_and_chain_specific_lengths() {
        let signature = bs58::encode([42u8; 64]).into_string();
        assert_eq!(
            transaction_hash(Chain::Solana, &signature).unwrap(),
            signature
        );
        assert!(transaction_hash(Chain::Ethereum, &signature).is_err());
        assert!(transaction_hash(Chain::Solana, &bs58::encode([42u8; 32]).into_string()).is_err());
        assert!(transaction_hash(Chain::Solana, &format!("0x{}", "ab".repeat(32))).is_err());
        for source_is_solana in [true, false] {
            let mut value = response("COMPLETE");
            let side = if source_is_solana {
                "srcChain"
            } else {
                "destChain"
            };
            value[side]["chainId"] = json!(chain_id(Chain::Solana));
            value[side]["txHash"] = json!(signature);
            value[side]["token"] = json!({
                "chainId": chain_id(Chain::Solana), "address": Chain::Solana.token(), "decimals": 6
            });
            let (source, destination, txid) = if source_is_solana {
                (Chain::Solana, Chain::Polygon, signature.clone())
            } else {
                (
                    Chain::Ethereum,
                    Chain::Solana,
                    format!("0x{}", "ab".repeat(32)),
                )
            };
            let status = serde_json::from_value::<StatusResponse>(value.clone())
                .unwrap()
                .into_status(source, destination, &txid)
                .unwrap();
            assert_eq!(status.state, "completed");
            if !source_is_solana {
                assert_eq!(status.destination_tx.as_deref(), Some(signature.as_str()));
                value["destChain"]["token"]["address"] =
                    json!(Chain::Solana.token().to_ascii_lowercase());
                assert_eq!(
                    serde_json::from_value::<StatusResponse>(value)
                        .unwrap()
                        .into_status(source, destination, &txid)
                        .unwrap()
                        .state,
                    "output unverified"
                );
            } else {
                value["srcChain"]["txHash"] = json!(bs58::encode([43u8; 64]).into_string());
                assert!(
                    serde_json::from_value::<StatusResponse>(value)
                        .unwrap()
                        .into_status(source, destination, &txid)
                        .is_err()
                );
            }
        }
    }
}
