use alloy::{
    primitives::{Address, U256},
    sol,
    sol_types::SolCall,
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;

use super::{Chain, ContractCall, address, amount, text};

sol! {
    function transfer(address recipient, uint256 amount) external;
    function approve(address spender, uint256 amount) external;
}

pub(super) fn normalize(
    tx: &Value,
    chain: Chain,
    sender: &str,
    units: U256,
    deadline: u64,
) -> Result<ContractCall> {
    ensure!(!units.is_zero(), "bridge input must be positive");
    let sender = address(chain, sender)?;
    let token = address(chain, chain.token())?;
    let (to, data, value) = transaction_call(&tx["trade"], chain, sender)?;
    let approval = if tx["approval"].is_null() {
        None
    } else {
        let (to, data, value) = transaction_call(&tx["approval"], chain, sender)?;
        ensure!(
            value.is_zero(),
            "approval must not transfer native currency"
        );
        Some((to, data))
    };
    ensure!(
        data.len() >= 4,
        "MetaMask transaction has no function selector"
    );
    let spender = if to == token {
        ensure!(
            approval.is_none() && value.is_zero(),
            "direct USDT transfer must not approve or send native currency"
        );
        // Relay appends a routing identifier to transfer calldata. The configured
        // USDT contract reads the canonical ABI prefix; retain the suffix untouched
        // for provider indexing. Never drop it or sign a remote transaction envelope.
        let prefix = data.get(..68).context("truncated direct USDT transfer")?;
        let transfer = transferCall::abi_decode(prefix).context("invalid direct USDT transfer")?;
        ensure!(
            prefix == transfer.abi_encode(),
            "noncanonical direct USDT transfer"
        );
        ensure!(
            transfer.recipient != Address::ZERO && transfer.amount == units,
            "direct USDT transfer recipient or amount mismatch"
        );
        None
    } else {
        if let Some((approval_to, data)) = approval {
            ensure!(
                approval_to == token,
                "MetaMask approval target is not configured USDT"
            );
            let approval =
                approveCall::abi_decode(&data).context("invalid MetaMask approval calldata")?;
            ensure!(
                data == approval.abi_encode(),
                "noncanonical MetaMask approval calldata"
            );
            ensure!(
                approval.amount == units,
                "MetaMask approval amount differs from exact requested input"
            );
            ensure!(
                approval.spender != Address::ZERO && approval.spender != token,
                "invalid MetaMask approval spender"
            );
            Some(approval.spender)
        } else {
            // An absent approval never identifies a spender. Do not grant a new
            // allowance; local RPC simulation must accept the existing allowance.
            None
        }
    };
    Ok(ContractCall {
        token: chain.token().to_owned(),
        spender: spender.map(|spender| encode_address(chain, spender)),
        amount: units,
        to: encode_address(chain, to),
        data,
        value,
        expires_at: Some(deadline),
    })
}

fn transaction_call(tx: &Value, chain: Chain, sender: Address) -> Result<(Address, Vec<u8>, U256)> {
    ensure!(tx.is_object(), "MetaMask returned no supported transaction");
    if chain == Chain::Tron {
        // Rebuild through the local RPC; never sign the provider's envelope.
        // A raw transaction memo may carry routing instructions, so dropping it
        // would change the bridge intent. Such routes are not executable here.
        let raw = &tx["raw_data"];
        ensure!(
            raw["data"].is_null() || raw["data"].as_str() == Some(""),
            "TRON route requires a transaction memo; cannot rebuild it as a contract-only call"
        );
        tron_call(raw, sender)
    } else {
        ensure!(
            tx["chainId"].as_u64() == Some(chain.chain_id().parse::<u64>()?),
            "MetaMask transaction chain ID mismatch"
        );
        ensure!(
            address(chain, text(tx, "from")?)? == sender,
            "MetaMask transaction sender mismatch"
        );
        ensure!(!tx["value"].is_null(), "missing MetaMask transaction value");
        Ok((
            address(chain, text(tx, "to")?)?,
            calldata(text(tx, "data")?)?,
            native_value(&tx["value"])?,
        ))
    }
}

fn calldata(input: &str) -> Result<Vec<u8>> {
    let input = input.strip_prefix("0x").unwrap_or(input);
    ensure!(
        !input.is_empty() && input.len() % 2 == 0,
        "invalid MetaMask calldata length"
    );
    hex::decode(input).context("invalid MetaMask hexadecimal calldata")
}

fn native_value(value: &Value) -> Result<U256> {
    if value.is_null() {
        return Ok(U256::ZERO);
    }
    if let Some(value) = value.as_u64() {
        return Ok(U256::from(value));
    }
    let value = value.as_str().context("invalid MetaMask native value")?;
    if let Some(hex) = value.strip_prefix("0x") {
        ensure!(
            !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid MetaMask native hexadecimal value"
        );
        U256::from_str_radix(hex, 16).context("MetaMask native value exceeds uint256")
    } else {
        amount::integer(value)
    }
}

fn tron_call(raw: &Value, sender: Address) -> Result<(Address, Vec<u8>, U256)> {
    let contracts = raw["contract"]
        .as_array()
        .context("missing TRON contract list")?;
    ensure!(
        contracts.len() == 1,
        "MetaMask TRON transaction must contain exactly one contract"
    );
    let contract = &contracts[0];
    ensure!(
        text(contract, "type")? == "TriggerSmartContract",
        "unsupported TRON contract type"
    );
    ensure!(
        text(&contract["parameter"], "type_url")?
            == "type.googleapis.com/protocol.TriggerSmartContract",
        "TRON parameter type mismatch"
    );
    for key in ["Permission_id", "permission_id"] {
        ensure!(
            native_value(&contract[key])?.is_zero(),
            "unsupported TRON transaction permission"
        );
    }
    let call = &contract["parameter"]["value"];
    ensure!(
        parse_tx_address(Chain::Tron, text(call, "owner_address")?)? == sender,
        "MetaMask TRON owner mismatch"
    );
    for key in ["call_token_value", "token_id"] {
        ensure!(
            native_value(&call[key])?.is_zero(),
            "unsupported TRON TRC10 token payment"
        );
    }
    let value = native_value(&call["call_value"])?;
    ensure!(
        value <= U256::from(i64::MAX as u64),
        "TRON call value exceeds signed protobuf range"
    );
    Ok((
        parse_tx_address(Chain::Tron, text(call, "contract_address")?)?,
        calldata(text(call, "data")?)?,
        value,
    ))
}

fn parse_tx_address(chain: Chain, input: &str) -> Result<Address> {
    if chain == Chain::Tron && !input.starts_with('T') {
        let hex = input.strip_prefix("0x").unwrap_or(input);
        let bytes = hex::decode(hex).context("invalid TRON hexadecimal address")?;
        ensure!(
            bytes.len() == 21 && bytes[0] == 0x41,
            "invalid TRON hexadecimal network prefix or length"
        );
        let address = Address::from_slice(&bytes[1..]);
        ensure!(address != Address::ZERO, "zero TRON address");
        Ok(address)
    } else {
        address(chain, input)
    }
}

fn encode_address(chain: Chain, address: Address) -> String {
    if chain == Chain::Tron {
        let mut bytes = [0u8; 21];
        bytes[0] = 0x41;
        bytes[1..].copy_from_slice(address.as_slice());
        bs58::encode(bytes).with_check().into_string()
    } else {
        address.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SENDER: &str = "0x6f33bb1763eebead07cf8815a62fcd7b30311fa3";
    const ROUTER: &str = "0x69460570c93f9DE5E2edbC3052bf10125f0Ca22d";
    const TRON_SENDER: &str = "TGmhZwFoCZYBcbJt82TmKQ3HXCmcV2UrGT";

    fn evm() -> Value {
        json!({
            "trade": {"chainId":1, "from":SENDER, "to":ROUTER,
                "data":"0x12345678", "value":"0x0"},
            "approval": {"chainId":1, "from":SENDER, "to":Chain::Ethereum.token(),
                "value":"0x0", "data":format!("0x{}", hex::encode(approveCall {
                    spender: address(Chain::Ethereum, ROUTER).unwrap(),
                    amount: U256::from(100)
                }.abi_encode()))}
        })
    }

    fn parse_evm(tx: &Value) -> Result<ContractCall> {
        normalize(tx, Chain::Ethereum, SENDER, U256::from(100), u64::MAX)
    }

    #[test]
    fn approvals_are_exact_and_bound_to_source_token() {
        let tx = evm();
        let call = parse_evm(&tx).unwrap();
        assert_eq!(call.amount, U256::from(100));
        assert_eq!(
            address(Chain::Ethereum, call.spender.as_deref().unwrap()).unwrap(),
            address(Chain::Ethereum, ROUTER).unwrap()
        );
        for units in [U256::ZERO, U256::from(99), U256::from(101), U256::MAX] {
            let mut bad = tx.clone();
            bad["approval"]["data"] = json!(format!(
                "0x{}",
                hex::encode(
                    approveCall {
                        spender: address(Chain::Ethereum, ROUTER).unwrap(),
                        amount: units
                    }
                    .abi_encode()
                )
            ));
            assert!(parse_evm(&bad).is_err());
        }
        let mut bad = tx;
        bad["approval"]["to"] = json!(ROUTER);
        assert!(parse_evm(&bad).is_err());
    }

    #[test]
    fn omitted_approval_never_implies_router_is_spender() {
        let mut tx = evm();
        tx["approval"] = Value::Null;
        let call = parse_evm(&tx).unwrap();
        assert!(call.spender.is_none());
        assert_eq!(call.amount, U256::from(100));
    }

    #[test]
    fn both_envelopes_bind_sender_chain_and_native_value() {
        for envelope in ["trade", "approval"] {
            for (field, value) in [
                ("from", json!(ROUTER)),
                ("chainId", json!(137)),
                ("value", json!("-1")),
                ("value", Value::Null),
            ] {
                let mut tx = evm();
                tx[envelope][field] = value;
                assert!(parse_evm(&tx).is_err(), "{envelope}.{field}");
            }
        }
        let mut tx = evm();
        tx["approval"]["value"] = json!("0x1");
        assert!(parse_evm(&tx).is_err());
        tx = evm();
        tx["trade"]["value"] = json!("0x2a");
        assert_eq!(parse_evm(&tx).unwrap().value, U256::from(42));
    }

    #[test]
    fn direct_deposit_checks_debit_and_preserves_routing_suffix() {
        let mut tx = evm();
        tx["trade"]["to"] = json!(Chain::Ethereum.token());
        tx["approval"] = Value::Null;
        let mut data = transferCall {
            recipient: address(Chain::Ethereum, ROUTER).unwrap(),
            amount: U256::from(100),
        }
        .abi_encode();
        data.extend_from_slice(&[42; 32]);
        tx["trade"]["data"] = json!(format!("0x{}", hex::encode(&data)));
        let call = parse_evm(&tx).unwrap();
        assert_eq!(call.data, data);
        assert!(call.spender.is_none());
        assert!(normalize(&tx, Chain::Ethereum, SENDER, U256::from(99), 0).is_err());
        tx["trade"]["value"] = json!("1");
        assert!(parse_evm(&tx).is_err());
    }

    fn tron() -> Value {
        let data = transferCall {
            recipient: address(Chain::Tron, TRON_SENDER).unwrap(),
            amount: U256::from(100),
        }
        .abi_encode();
        json!({"trade": {"raw_data":{"contract":[{
            "type":"TriggerSmartContract", "parameter":{
                "type_url":"type.googleapis.com/protocol.TriggerSmartContract",
                "value":{"owner_address":"414a9bbc3fe169c752a08de9c7becfb4921034ae4a",
                    "contract_address":"41a614f803b6fd780986a42c78ec9c7f77e6ded13c",
                    "data":hex::encode(data), "call_value":0}
            }
        }]}}})
    }

    #[test]
    fn tron_rejects_memos_multiple_calls_and_changed_owner() {
        let tx = tron();
        let parse = |tx: &Value| normalize(tx, Chain::Tron, TRON_SENDER, U256::from(100), u64::MAX);
        let call = parse(&tx).unwrap();
        assert_eq!(call.to, Chain::Tron.token());
        assert_eq!(call.amount, U256::from(100));
        let mut bad = tx.clone();
        bad["trade"]["raw_data"]["data"] = json!("08001111");
        assert!(parse(&bad).is_err());
        bad = tx.clone();
        bad["trade"]["raw_data"]["contract"]
            .as_array_mut()
            .unwrap()
            .push(tx["trade"]["raw_data"]["contract"][0].clone());
        assert!(parse(&bad).is_err());
        for (field, value) in [
            (
                "owner_address",
                json!("41a614f803b6fd780986a42c78ec9c7f77e6ded13c"),
            ),
            (
                "contract_address",
                json!("00a614f803b6fd780986a42c78ec9c7f77e6ded13c"),
            ),
            ("call_value", json!("9223372036854775808")),
            ("call_token_value", json!(1)),
        ] {
            bad = tx.clone();
            bad["trade"]["raw_data"]["contract"][0]["parameter"]["value"][field] = value;
            assert!(parse(&bad).is_err(), "{field}");
        }
    }

    #[test]
    fn tron_approvals_decode_spender_and_reject_excess_amount() {
        let mut tx = tron();
        let router = address(Chain::Tron, TRON_SENDER).unwrap();
        tx["approval"] = tx["trade"].clone();
        tx["trade"]["raw_data"]["contract"][0]["parameter"]["value"]["contract_address"] =
            json!(TRON_SENDER);
        tx["trade"]["raw_data"]["contract"][0]["parameter"]["value"]["data"] = json!("12345678");
        for (units, valid) in [
            (U256::from(100), true),
            (U256::MAX, false),
            (U256::ZERO, false),
        ] {
            tx["approval"]["raw_data"]["contract"][0]["parameter"]["value"]["data"] =
                json!(hex::encode(
                    approveCall {
                        spender: router,
                        amount: units
                    }
                    .abi_encode()
                ));
            let result = normalize(&tx, Chain::Tron, TRON_SENDER, U256::from(100), u64::MAX);
            assert_eq!(result.is_ok(), valid);
            if let Ok(call) = result {
                assert_eq!(call.spender.as_deref(), Some(TRON_SENDER));
            }
        }
    }

    #[test]
    fn polygon_requires_its_own_token_and_chain() {
        let mut tx = evm();
        tx["trade"]["chainId"] = json!(137);
        tx["approval"]["chainId"] = json!(137);
        tx["approval"]["to"] = json!(Chain::Polygon.token());
        let call = normalize(&tx, Chain::Polygon, SENDER, U256::from(100), u64::MAX).unwrap();
        assert_eq!(call.token, Chain::Polygon.token());
        tx["approval"]["to"] = json!(Chain::Ethereum.token());
        assert!(normalize(&tx, Chain::Polygon, SENDER, U256::from(100), u64::MAX).is_err());
    }
}
