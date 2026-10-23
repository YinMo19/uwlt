//! MetaMask v1 Solana `trade` is a base64 serialized transaction, not EVM calldata.
//! Schema: https://github.com/MetaMask/core/blob/main/packages/bridge-controller/src/validators/quote-response-v1.ts
//! Only the observed xoswap direct-deposit shape is supported. The provider remains
//! trusted for the deposit-account mapping to the quoted cross-chain recipient.
//! Instructions are preserved byte-for-byte; unsupported routers/memos fail closed.
use std::str::FromStr;

use alloy::primitives::U256;
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use solana_message::VersionedMessage;
use solana_pubkey::Pubkey;
use solana_signature::Signature;

use super::{amount, text};
use crate::solana::{RemoteRoute, RouteTransfer, USDT};

pub(super) fn normalize(
    response: &Value,
    sender: &str,
    recipient: &str,
    units: U256,
    deadline: u64,
) -> Result<RemoteRoute> {
    let quote = &response["quote"];
    ensure!(
        text(quote, "bridgeId")? == "xoswap",
        "unsupported Solana bridge provider: only decoded xoswap direct-deposit routes are supported; opaque router instructions are never signed"
    );
    ensure!(
        response["approval"].is_null() && response["resetApproval"].is_null(),
        "Solana bridge route must not request approvals"
    );
    let encoded = text(response, "trade")?;
    let tx = crate::solana::decode_transaction(encoded)?;
    tx.sanitize().context("malformed Solana bridge message")?;
    ensure!(
        tx.signatures.len() == 1 && tx.signatures[0] == Signature::default(),
        "Solana bridge transaction must be unsigned and require exactly one signer"
    );
    let sender_key = Pubkey::from_str(sender).context("invalid Solana bridge sender")?;
    let mint = Pubkey::from_str(USDT)?;
    let (header, keys, instructions) = match &tx.message {
        VersionedMessage::Legacy(message) => (
            &message.header,
            &message.account_keys,
            &message.instructions,
        ),
        VersionedMessage::V0(message) => {
            ensure!(
                message.address_table_lookups.is_empty(),
                "Solana deposit route with address lookup tables is unsupported"
            );
            (
                &message.header,
                &message.account_keys,
                &message.instructions,
            )
        }
    };
    ensure!(
        header.num_required_signatures == 1 && keys.first() == Some(&sender_key),
        "Solana bridge fee payer or signer mismatch"
    );
    // Observed live getQuote shape: two compute-budget instructions, ATA create,
    // routed USDT transfer, MetaMask USDT fee transfer. No instructions are dropped.
    ensure!(
        instructions.len() == 5,
        "unsupported Solana deposit instruction count"
    );
    let program = |index: usize| -> Result<Pubkey> {
        keys.get(instructions[index].program_id_index as usize)
            .copied()
            .context("invalid Solana program account index")
    };
    let accounts = |index: usize| -> Result<Vec<Pubkey>> {
        instructions[index]
            .accounts
            .iter()
            .map(|index| {
                keys.get(*index as usize)
                    .copied()
                    .context("invalid Solana instruction account index")
            })
            .collect()
    };
    let compute = Pubkey::from_str("ComputeBudget111111111111111111111111111111")?;
    let mut price = false;
    let mut limit = false;
    for index in 0..2 {
        let ix = &instructions[index];
        ensure!(
            program(index)? == compute && ix.accounts.is_empty(),
            "unsupported Solana deposit compute-budget instruction"
        );
        match ix.data.as_slice() {
            [3, rest @ ..] if rest.len() == 8 && !price => price = true,
            [2, rest @ ..] if rest.len() == 4 && !limit => limit = true,
            _ => anyhow::bail!("unsupported or duplicate Solana compute-budget setting"),
        }
    }
    ensure!(price && limit, "missing Solana compute-budget settings");
    let create = &instructions[2];
    ensure!(
        program(2)? == spl_associated_token_account::id()
            && (create.data.is_empty() || create.data == [1]),
        "Solana deposit requires a standard associated-token-account create instruction"
    );
    let create_accounts = accounts(2)?;
    ensure!(
        create_accounts.len() == 6,
        "unsupported Solana deposit ATA account layout"
    );
    let deposit = create_accounts[1];
    let deposit_owner = create_accounts[2];
    ensure!(
        deposit_owner != sender_key && deposit_owner != Pubkey::default(),
        "invalid Solana bridge deposit owner"
    );
    ensure!(
        create_accounts[0] == sender_key
            && create_accounts[3] == mint
            && create_accounts[4] == Pubkey::default()
            && create_accounts[5] == spl_token::id()
            && deposit
                == spl_associated_token_account::get_associated_token_address(
                    &deposit_owner,
                    &mint
                ),
        "Solana bridge deposit ATA, mint, payer or program mismatch"
    );
    let fee = &quote["feeData"]["metabridge"];
    let fee_owner = Pubkey::from_str(text(fee, "recipient")?)
        .context("invalid Solana MetaMask fee recipient")?;
    ensure!(
        fee_owner != Pubkey::default() && fee_owner != sender_key && fee_owner != deposit_owner,
        "invalid Solana MetaMask fee recipient"
    );
    let fee_account = spl_associated_token_account::get_associated_token_address(&fee_owner, &mint);
    let source_account =
        spl_associated_token_account::get_associated_token_address(&sender_key, &mint);
    let routed = to_u64(amount::integer(text(quote, "srcTokenAmount")?)?)?;
    let fee_amount = to_u64(amount::integer(text(fee, "amount")?)?)?;
    let input_amount = to_u64(units)?;
    ensure!(
        routed > 0 && routed.checked_add(fee_amount) == Some(input_amount),
        "Solana deposit and fee do not equal requested input"
    );
    let transfers = vec![
        RouteTransfer {
            destination: deposit.to_string(),
            amount: routed,
        },
        RouteTransfer {
            destination: fee_account.to_string(),
            amount: fee_amount,
        },
    ];
    for (index, transfer) in transfers.iter().enumerate() {
        let ix_index = index + 3;
        ensure!(
            program(ix_index)? == spl_token::id(),
            "unsupported Solana bridge transfer program"
        );
        ensure!(
            accounts(ix_index)?
                == [
                    source_account,
                    Pubkey::from_str(&transfer.destination)?,
                    sender_key
                ],
            "Solana bridge token source, destination or authority mismatch"
        );
        let data = &instructions[ix_index].data;
        ensure!(
            data.len() == 9 && data[0] == 3,
            "Solana bridge requires an exact classic SPL transfer, not approval or authority changes"
        );
        let value = u64::from_le_bytes(data[1..].try_into()?);
        ensure!(
            value == transfer.amount,
            "Solana bridge transfer amount mismatch"
        );
    }
    Ok(RemoteRoute {
        transaction: encoded.to_owned(),
        from: sender.to_owned(),
        input_mint: USDT.to_owned(),
        input_amount,
        recipient: recipient.to_owned(),
        expires_at: deadline,
        last_valid_block_height: None,
        transfers,
    })
}

fn to_u64(value: U256) -> Result<u64> {
    value
        .try_into()
        .context("Solana token amount exceeds uint64")
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde_json::json;
    use solana_transaction::versioned::VersionedTransaction;

    const SENDER: &str = "9xQeWvG816bUx9EPfEZzxAQbBqFQvcwACV9j8CjKzG77";
    const RECIPIENT: &str = "0x6f33bb1763eebead07cf8815a62fcd7b30311fa3";
    // Live /getQuote xoswap Solana->Ethereum response, 100 USDT, 50 bps.
    const TRADE: &str = "AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAACAAQAGCoUPLW4CpHr4JNCatXqw8fsGhLurVGsqqxxveMBlqjEGV18FW/zqI+S+fmQoco/iv2xLhNTsjtJAm69Tau7eGZVaphhVa1+bFyJpqoqU+OHKntLzz2ncke6/mbvy2rSgjeSnq2A/eZ8euZwna4vHcN0CWTnuB65dTjFg2Tk1cDA5AwZGb+UhFzL/7K26csOb57yM5bvF9xJrLEObOkAAAACMlyWPTiSJ8bs9ECkUjg2DC1oTmdr/EIQEjnvY2+n4WfboYYnWFDJW97RmmF1SGSCRj7uP2haLZKX/hYHPMXaizgEOYK/tsicXvWMZL1QUWj+WWjO7gtLHAp6yzh4ggmQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAbd9uHXZaGT2cvhRs7reawctIXtX1s3kTqM9YV+/wCp1K+VUUUUOkAWzB/D+ib6C8rsTHBMFM6p3DRcY4NVfx4FBAAJA8GnAAAAAAAABAAFAn8aBgAFBgABBgcICQAJAwIBAAkDCIfoBQAAAAAJAwIDAAkD+FkNAAAAAAAA";

    fn fixture() -> Value {
        json!({
            "quote": {
                "bridgeId": "xoswap", "srcTokenAmount": "99125000",
                "feeData": {"metabridge": {"amount": "875000",
                    "recipient": "4cLUBQKZgCv2AqGXbh8ncGhrDRcicUe3WSDzjgPY2oTA"}}
            },
            "trade": TRADE
        })
    }

    fn parse(value: &Value) -> Result<RemoteRoute> {
        normalize(value, SENDER, RECIPIENT, U256::from(100_000_000), u64::MAX)
    }

    fn mutate(change: impl FnOnce(&mut VersionedTransaction)) -> Value {
        let mut tx = bincode::deserialize(&STANDARD.decode(TRADE).unwrap()).unwrap();
        change(&mut tx);
        let mut value = fixture();
        value["trade"] = json!(STANDARD.encode(bincode::serialize(&tx).unwrap()));
        value
    }

    #[test]
    fn observed_deposit_preserves_transaction_and_exact_input() {
        let route = parse(&fixture()).unwrap();
        assert_eq!(route.transaction, TRADE);
        assert_eq!(route.input_amount, 100_000_000);
        assert_eq!(route.transfers[0].amount, 99_125_000);
        assert_eq!(route.transfers[1].amount, 875_000);
        assert_eq!(route.recipient, RECIPIENT);
    }

    #[test]
    fn rejects_altered_fee_and_approval_envelopes() {
        for (field, value) in [("recipient", json!(SENDER)), ("amount", json!("875001"))] {
            let mut candidate = fixture();
            candidate["quote"]["feeData"]["metabridge"][field] = value;
            assert!(parse(&candidate).is_err());
        }
        for field in ["approval", "resetApproval"] {
            let mut candidate = fixture();
            candidate[field] = json!({});
            assert!(parse(&candidate).is_err());
        }
        let mut candidate = fixture();
        candidate["quote"]["bridgeId"] = json!("opaque-router");
        assert!(parse(&candidate).is_err());
    }

    #[test]
    fn rejects_modified_amount_authority_instructions_and_signatures() {
        for mutation in 0..6 {
            let candidate = mutate(|tx| {
                let VersionedMessage::V0(message) = &mut tx.message else {
                    panic!("v0 fixture")
                };
                match mutation {
                    0 => message.instructions[3].data[1] ^= 1,
                    1 => message.instructions[3].accounts[2] = 6,
                    2 => message.instructions[3].data[0] = 4,
                    3 => message.instructions.push(message.instructions[3].clone()),
                    4 => message.account_keys[0] = Pubkey::new_from_array([7; 32]),
                    _ => tx.signatures[0] = Signature::from([1; 64]),
                }
            });
            assert!(parse(&candidate).is_err(), "mutation {mutation}");
        }
    }
}
