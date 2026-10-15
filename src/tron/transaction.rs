use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use super::Asset;

pub(super) fn address_hex(address: &str) -> Result<String> {
    let bytes = bs58::decode(address)
        .with_check(None)
        .into_vec()
        .context("invalid TRON address checksum or Base58 encoding")?;
    ensure!(
        bytes.len() == 21 && bytes[0] == 0x41,
        "invalid TRON address prefix or length"
    );
    Ok(hex::encode(bytes))
}

pub(super) fn address_parameter(address: &str) -> String {
    // ABI uses the 20-byte EVM address, not TRON's leading 0x41 network byte.
    format!("{:0>64}", &address[2..])
}

pub(super) fn transfer_parameter(to: &str, amount: u64) -> String {
    format!("{}{:064x}", address_parameter(to), amount)
}

pub(super) fn decode_word(value: &str) -> Result<u64> {
    let mut word = [0_u8; 32];
    hex::decode_to_slice(value, &mut word).context("invalid contract result hex")?;
    ensure!(
        word[..24].iter().all(|byte| *byte == 0),
        "contract amount exceeds u64 range"
    );
    let mut low = [0_u8; 8];
    low.copy_from_slice(&word[24..]);
    Ok(u64::from_be_bytes(low))
}

pub(super) fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("node response missing string {key}"))
}

pub(super) fn unsigned(value: &Value, key: &str) -> Result<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .with_context(|| format!("node response missing unsigned integer {key}"))
}

pub(super) fn default_zero(value: &Value, key: &str) -> Result<u64> {
    // Protobuf JSON omits scalar fields equal to their default value.
    match value.get(key) {
        None => Ok(0),
        Some(field) => field
            .as_u64()
            .with_context(|| format!("invalid unsigned integer {key}")),
    }
}

pub(super) fn same_hex(value: &Value, key: &str, expected: &str) -> Result<()> {
    ensure!(
        text(value, key)?.eq_ignore_ascii_case(expected),
        "node returned mismatching {key}"
    );
    Ok(())
}

pub(super) struct Intent<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub amount: u64,
    pub asset: Asset,
    pub token: &'a str,
    pub fee_limit: u64,
}

/// Check authorized intent here; Wallet Core verifies transaction encoding and hashes before signing.
pub(super) fn validate(transaction: &Value, intent: &Intent<'_>) -> Result<()> {
    validate_inner(transaction, intent, None, 0)
}

/// Arbitrary router/approval calls share every transfer envelope and permission check.
pub(super) fn validate_call(
    transaction: &Value,
    from: &str,
    contract: &str,
    data: &str,
    fee_limit: u64,
    call_value: u64,
) -> Result<()> {
    validate_inner(
        transaction,
        &Intent {
            from,
            to: from,
            amount: 0,
            asset: Asset::Usdt,
            token: contract,
            fee_limit,
        },
        Some(data),
        call_value,
    )
}

fn validate_inner(
    transaction: &Value,
    intent: &Intent<'_>,
    calldata: Option<&str>,
    call_value: u64,
) -> Result<()> {
    let raw = transaction
        .get("raw_data")
        .context("node transaction missing raw_data")?;
    let contracts = raw
        .get("contract")
        .and_then(Value::as_array)
        .context("node transaction missing contracts")?;
    ensure!(
        contracts.len() == 1,
        "node transaction must contain exactly one contract"
    );
    let contract = &contracts[0];
    ensure!(
        default_zero(contract, "Permission_id")? == 0,
        "node changed signing permission"
    );
    let parameter = contract
        .get("parameter")
        .context("node contract missing parameter")?;
    let value = parameter
        .get("value")
        .and_then(Value::as_object)
        .context("node contract missing parameter value")?;
    let parameter_value = &parameter["value"];
    same_hex(parameter_value, "owner_address", intent.from)?;
    let (kind, fields): (&str, &[&str]) = match intent.asset {
        Asset::Trx => {
            same_hex(parameter_value, "to_address", intent.to)?;
            ensure!(
                unsigned(parameter_value, "amount")? == intent.amount,
                "node changed TRX amount"
            );
            ensure!(
                default_zero(raw, "fee_limit")? == 0,
                "TRX transfer unexpectedly has fee_limit"
            );
            (
                "TransferContract",
                &["owner_address", "to_address", "amount"],
            )
        }
        Asset::Usdt => {
            same_hex(parameter_value, "contract_address", intent.token)?;
            let transfer_data;
            let data = if let Some(data) = calldata {
                data
            } else {
                transfer_data = format!("a9059cbb{}", transfer_parameter(intent.to, intent.amount));
                &transfer_data
            };
            same_hex(parameter_value, "data", data)?;
            ensure!(
                default_zero(parameter_value, "call_value")? == call_value,
                "node changed native call value"
            );
            for key in ["call_token_value", "token_id"] {
                ensure!(
                    default_zero(parameter_value, key)? == 0,
                    "node added unwanted {key}"
                );
            }
            ensure!(
                unsigned(raw, "fee_limit")? == intent.fee_limit,
                "node changed fee_limit"
            );
            (
                "TriggerSmartContract",
                &[
                    "owner_address",
                    "contract_address",
                    "data",
                    "call_value",
                    "call_token_value",
                    "token_id",
                ],
            )
        }
    };
    ensure!(
        text(contract, "type")? == kind,
        "node changed contract type"
    );
    ensure!(
        text(parameter, "type_url")? == format!("type.googleapis.com/protocol.{kind}"),
        "node changed contract parameter type"
    );
    for key in value.keys() {
        ensure!(
            fields.contains(&key.as_str()),
            "node added unexpected contract field {key}"
        );
    }
    for key in ["data", "scripts"] {
        if let Some(field) = raw.get(key) {
            ensure!(
                field.as_str() == Some(""),
                "node added unwanted transaction {key}"
            );
        }
    }
    if let Some(auths) = raw.get("auths") {
        ensure!(
            auths.as_array().is_some_and(Vec::is_empty),
            "node added transaction authorities"
        );
    }
    if let Some(visible) = transaction.get("visible") {
        ensure!(
            visible.as_bool() == Some(false),
            "node changed transaction address encoding"
        );
    }
    if let Some(signatures) = transaction.get("signature") {
        ensure!(
            signatures.as_array().is_some_and(Vec::is_empty),
            "node returned an already signed transaction"
        );
    }
    Ok(())
}

pub(super) fn validate_txid(txid: &str) -> Result<()> {
    ensure!(
        txid.len() == 64 && txid.bytes().all(|b| b.is_ascii_hexdigit()),
        "transaction ID must be 64 hexadecimal characters"
    );
    Ok(())
}

pub(super) fn node_message(value: &Value) -> String {
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("RPC_ERROR");
    let message = value
        .get("message")
        .or_else(|| value.get("resMessage"))
        .or_else(|| value.get("Error"))
        .and_then(Value::as_str)
        .unwrap_or("node did not provide a reason");
    format!("{code}: {}", decode_message(message))
}

pub(super) fn decode_message(message: &str) -> String {
    hex::decode(message)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| message.to_owned())
}

pub(super) fn contract_success(response: &Value) -> Result<()> {
    let result = response
        .get("result")
        .context("node contract response missing result")?;
    if result.get("result").and_then(Value::as_bool) != Some(true) {
        bail!("contract request failed: {}", node_message(result));
    }
    if let Some(returns) = response.pointer("/transaction/ret") {
        let returns = returns
            .as_array()
            .context("invalid simulation execution results")?;
        for execution in returns {
            for key in ["ret", "contractRet"] {
                if let Some(state) = execution.get(key) {
                    ensure!(
                        matches!(state.as_str(), Some("SUCCESS" | "SUCESS")),
                        "contract execution failed: {state}; {}",
                        node_message(result)
                    );
                }
            }
        }
    }
    Ok(())
}

pub(super) fn transfer_success(
    response: &Value,
    from: &str,
    to: &str,
    token: &str,
    amount: u64,
) -> Result<()> {
    contract_success(response)?;
    // TRON USDT returns a zero word on successful transfer; use the actual event.
    let topics = serde_json::json!([
        "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
        address_parameter(from),
        address_parameter(to)
    ]);
    let data = format!("{amount:064x}");
    let logs = response
        .get("logs")
        .and_then(Value::as_array)
        .context("transfer simulation has no event logs")?;
    ensure!(
        logs.iter().any(|log| {
            log["address"].as_str() == Some(&token[2..])
                && log["topics"] == topics
                && log["data"].as_str() == Some(data.as_str())
        }),
        "simulation did not emit the requested USDT transfer"
    );
    Ok(())
}

pub(super) fn constant_word(response: &Value) -> Result<u64> {
    let results = response
        .get("constant_result")
        .and_then(Value::as_array)
        .context("node response missing constant_result")?;
    ensure!(results.len() == 1, "expected one contract return value");
    decode_word(
        results[0]
            .as_str()
            .context("invalid constant_result value")?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn transaction() -> Value {
        json!({
            "raw_data": {
                "fee_limit": 30_000_000,
                "contract": [{
                    "type": "TriggerSmartContract",
                    "parameter": {
                        "type_url": "type.googleapis.com/protocol.TriggerSmartContract",
                        "value": {
                            "owner_address": "410000000000000000000000000000000000000001",
                            "contract_address": "410000000000000000000000000000000000000003",
                            "data": format!(
                                "a9059cbb{}",
                                transfer_parameter(
                                    "410000000000000000000000000000000000000002",
                                    1_000_000
                                )
                            )
                        }
                    }
                }]
            }
        })
    }

    fn intent() -> Intent<'static> {
        Intent {
            from: "410000000000000000000000000000000000000001",
            to: "410000000000000000000000000000000000000002",
            token: "410000000000000000000000000000000000000003",
            amount: 1_000_000,
            asset: Asset::Usdt,
            fee_limit: 30_000_000,
        }
    }

    #[test]
    fn rejects_node_changes_to_authorized_transfer() {
        assert!(validate(&transaction(), &intent()).is_ok());
        for (pointer, replacement) in [
            (
                "/raw_data/contract/0/parameter/value/owner_address",
                json!(intent().to),
            ),
            (
                "/raw_data/contract/0/parameter/value/contract_address",
                json!(intent().to),
            ),
            (
                "/raw_data/contract/0/parameter/value/data",
                json!(format!(
                    "a9059cbb{}",
                    transfer_parameter(intent().to, 1_000_001)
                )),
            ),
            ("/raw_data/fee_limit", json!(30_000_001)),
            ("/raw_data/contract/0/type", json!("TransferAssetContract")),
        ] {
            let mut tx = transaction();
            *tx.pointer_mut(pointer).unwrap() = replacement;
            assert!(
                validate(&tx, &intent()).is_err(),
                "accepted mutation {pointer}"
            );
        }
        for (key, value) in [
            ("call_value", json!(1)),
            ("call_token_value", json!(1)),
            ("token_id", json!(1)),
            ("amount", json!(1)),
        ] {
            let mut tx = transaction();
            tx["raw_data"]["contract"][0]["parameter"]["value"][key] = value;
            assert!(validate(&tx, &intent()).is_err(), "accepted {key}");
        }
        let mut tx = transaction();
        tx["raw_data"]["contract"][0]["Permission_id"] = json!(2);
        assert!(validate(&tx, &intent()).is_err());
        let mut tx = transaction();
        let extra = tx["raw_data"]["contract"][0].clone();
        tx["raw_data"]["contract"]
            .as_array_mut()
            .unwrap()
            .push(extra);
        assert!(validate(&tx, &intent()).is_err());
    }

    #[test]
    fn rejects_native_transfer_redirection() {
        let intent = Intent {
            asset: Asset::Trx,
            ..intent()
        };
        let tx = json!({
            "raw_data": {
                "contract": [{
                    "type": "TransferContract",
                    "parameter": {
                        "type_url": "type.googleapis.com/protocol.TransferContract",
                        "value": {
                            "owner_address": intent.from,
                            "to_address": intent.to,
                            "amount": intent.amount
                        }
                    }
                }]
            }
        });
        assert!(validate(&tx, &intent).is_ok());
        for (key, replacement) in [
            ("to_address", json!(intent.from)),
            ("amount", json!(intent.amount + 1)),
        ] {
            let mut changed = tx.clone();
            changed["raw_data"]["contract"][0]["parameter"]["value"][key] = replacement;
            assert!(
                validate(&changed, &intent).is_err(),
                "accepted changed {key}"
            );
        }
        let mut changed = tx;
        changed["raw_data"]["fee_limit"] = json!(1);
        assert!(validate(&changed, &intent).is_err());
    }

    #[test]
    fn abi_is_exact_and_detects_overflow() {
        assert_eq!(
            decode_word(&format!("{:064x}", u64::MAX)).unwrap(),
            u64::MAX
        );
        assert!(decode_word(&format!("{:064x}", u128::from(u64::MAX) + 1)).is_err());
    }

    #[test]
    fn distinguishes_api_success_from_execution_failure() {
        assert!(
            contract_success(
                &json!({"result": {"result": true}, "transaction": {"ret": [{"ret": "FAILED"}]}})
            )
            .is_err()
        );
        assert!(contract_success(&json!({"result": {"result": false}})).is_err());
    }

    #[test]
    fn accepts_legacy_usdt_return_only_with_the_expected_transfer_effect() {
        let intent = intent();
        let mut response = json!({
            "result": {"result": true},
            "transaction": {"ret": [{}]},
            "constant_result": ["00".repeat(32)],
            "logs": [{
                "address": &intent.token[2..],
                "topics": [
                    "ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
                    address_parameter(intent.from), address_parameter(intent.to)
                ],
                "data": format!("{:064x}", intent.amount)
            }]
        });
        assert!(
            transfer_success(
                &response,
                intent.from,
                intent.to,
                intent.token,
                intent.amount
            )
            .is_ok()
        );
        response["logs"][0]["data"] = json!(format!("{:064x}", intent.amount + 1));
        assert!(
            transfer_success(
                &response,
                intent.from,
                intent.to,
                intent.token,
                intent.amount
            )
            .is_err()
        );
    }
}
