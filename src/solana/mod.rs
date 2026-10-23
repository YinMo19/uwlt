mod amount;
mod route;
#[cfg(test)]
#[path = "tests.rs"]
mod rpc_tests;
mod status;
mod swap;

pub(crate) use route::decode_transaction;
pub use route::{RemoteRoute, RouteTransfer};
pub use status::{TxState, TxStatus};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use solana_hash::Hash;
use solana_instruction::Instruction;
use solana_message::{Message, VersionedMessage};
use solana_program_option::COption;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_system_interface::{instruction as system_instruction, program as system_program};
use solana_transaction::versioned::VersionedTransaction;
use spl_token::state::{Account as TokenAccount, AccountState, Mint};
use std::{
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const USDT: &str = "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB";
const MAINNET_GENESIS: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asset {
    Usdt,
    Native,
}
pub struct Balances {
    pub usdt: String,
    pub native: String,
}
pub struct FeeRates {
    pub native_transfer: String,
    pub token_account_rent: String,
}

/// The exact message is immutable between preview, authentication and signing.
pub struct Prepared {
    message: VersionedMessage,
    sender: Pubkey,
    last_valid_block_height: Option<u64>,
    pub expires_at: u64,
    pub from: String,
    pub to: String,
    pub amount: String,
    pub symbol: String,
    pub fee: String,
    pub rent: String,
}
pub struct Signed {
    pub txid: String,
    raw: Vec<u8>,
    blockhash: Hash,
    last_valid_block_height: Option<u64>,
    expires_at: u64,
}
pub struct Client {
    http: reqwest::blocking::Client,
    endpoint: reqwest::Url,
    token: Pubkey,
}
struct Account {
    owner: Pubkey,
    lamports: u64,
    data: Vec<u8>,
    executable: bool,
}

impl Client {
    pub fn new(endpoint: Option<&str>) -> Result<Self> {
        let client = Self {
            http: reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()?,
            endpoint: endpoint
                .unwrap_or("https://api.mainnet-beta.solana.com")
                .parse()
                .context("invalid Solana RPC URL")?,
            token: address(USDT)?,
        };
        client.verify_network()?;
        Ok(client)
    }
    pub fn native_symbol(&self) -> &'static str {
        "SOL"
    }
    pub fn token_symbol(&self) -> &'static str {
        "USDT"
    }
    fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let mut response: Value = self
            .http
            .post(self.endpoint.clone())
            .json(&json!({"jsonrpc":"2.0", "id":1, "method":method, "params":params}))
            .send()
            .with_context(|| format!("Solana {method} transport failed"))?
            .error_for_status()?
            .json()?;
        if let Some(error) = response.get("error") {
            bail!("Solana {method}: {error}");
        }
        response
            .get_mut("result")
            .map(Value::take)
            .with_context(|| format!("Solana {method} response has no result"))
    }
    fn verify_network(&self) -> Result<()> {
        ensure!(
            self.rpc("getGenesisHash", json!([]))?.as_str() == Some(MAINNET_GENESIS),
            "Solana RPC genesis hash does not match mainnet"
        );
        Ok(())
    }
    fn account(&self, key: &Pubkey) -> Result<Option<Account>> {
        let result = self.rpc(
            "getAccountInfo",
            json!([key.to_string(), {"encoding":"base64", "commitment":"confirmed"}]),
        )?;
        if result["value"].is_null() {
            return Ok(None);
        }
        Ok(Some(decode_account(&result["value"])?))
    }
    fn native_balance(&self, key: &Pubkey) -> Result<u64> {
        self.rpc(
            "getBalance",
            json!([key.to_string(), {"commitment":"confirmed"}]),
        )?["value"]
            .as_u64()
            .context("invalid Solana balance")
    }
    fn validate_wallet(&self, key: &Pubkey) -> Result<()> {
        ensure!(key.is_on_curve(), "sender must be an Ed25519 wallet");
        if let Some(account) = self.account(key)? {
            ensure!(
                account.owner == system_program::id()
                    && account.data.is_empty()
                    && !account.executable,
                "sender is not a system wallet account"
            );
        }
        Ok(())
    }
    fn mint(&self) -> Result<Pubkey> {
        self.validate_mint(&self.token, 6)?;
        Ok(self.token)
    }
    fn validate_mint(&self, key: &Pubkey, decimals: u8) -> Result<()> {
        let account = self.account(key)?.context("token mint does not exist")?;
        ensure!(
            account.owner == spl_token::id() && !account.executable,
            "mint must belong to the classic SPL Token program"
        );
        let mint = Mint::unpack(&account.data).context("invalid classic SPL mint")?;
        ensure!(
            mint.is_initialized && mint.decimals == decimals,
            "token mint has incorrect decimals or is uninitialized"
        );
        Ok(())
    }
    fn token_accounts(&self, owner: &Pubkey, mint: &Pubkey) -> Result<Vec<(Pubkey, TokenAccount)>> {
        let result = self.rpc("getTokenAccountsByOwner", json!([owner.to_string(), {"mint":mint.to_string()}, {"encoding":"base64", "commitment":"confirmed"}]))?;
        let mut accounts = Vec::new();
        for entry in result["value"]
            .as_array()
            .context("invalid token account list")?
        {
            let key = address(
                entry["pubkey"]
                    .as_str()
                    .context("token account lacks address")?,
            )?;
            let account = checked_token(&decode_account(&entry["account"])?, mint, Some(owner))?;
            ensure!(
                !accounts.iter().any(|(other, _)| *other == key),
                "RPC returned duplicate token accounts"
            );
            accounts.push((key, account));
        }
        accounts.sort_by_key(|(key, _)| *key);
        Ok(accounts)
    }
    pub fn balance(&self, address_text: &str) -> Result<Balances> {
        let owner = address(address_text)?;
        let native = amount::format(self.native_balance(&owner)?, 9);
        let mint = self.mint()?;
        let accounts = self.token_accounts(&owner, &mint)?;
        let total = accounts.iter().try_fold(0u64, |total, (_, account)| {
            total
                .checked_add(account.amount)
                .context("token balance overflow")
        })?;
        let usdt = amount::format(total, 6);
        Ok(Balances { usdt, native })
    }
    fn latest_blockhash(&self) -> Result<(Hash, u64)> {
        let value = self.rpc("getLatestBlockhash", json!([{"commitment":"confirmed"}]))?;
        Ok((
            Hash::from_str(
                value["value"]["blockhash"]
                    .as_str()
                    .context("missing blockhash")?,
            )?,
            value["value"]["lastValidBlockHeight"]
                .as_u64()
                .context("missing blockhash expiry")?,
        ))
    }
    fn rent_lamports(&self) -> Result<u64> {
        self.rpc(
            "getMinimumBalanceForRentExemption",
            json!([TokenAccount::LEN, {"commitment":"confirmed"}]),
        )?
        .as_u64()
        .context("invalid rent exemption response")
    }
    fn message_fee(&self, message: &VersionedMessage) -> Result<u64> {
        self.rpc(
            "getFeeForMessage",
            json!([STANDARD.encode(message.serialize()), {"commitment":"confirmed"}]),
        )?["value"]
            .as_u64()
            .context("message blockhash expired or fee unavailable")
    }
    pub fn fees(&self) -> Result<FeeRates> {
        let (hash, _) = self.latest_blockhash()?;
        let payer = Pubkey::new_from_array([1; 32]);
        let message = VersionedMessage::Legacy(Message::new_with_blockhash(
            &[system_instruction::transfer(
                &payer,
                &Pubkey::new_from_array([2; 32]),
                1,
            )],
            Some(&payer),
            &hash,
        ));
        Ok(FeeRates {
            native_transfer: amount::format(self.message_fee(&message)?, 9),
            token_account_rent: amount::format(self.rent_lamports()?, 9),
        })
    }
    pub fn prepare(&self, from: &str, to: &str, value: &str, asset: Asset) -> Result<Prepared> {
        let sender = address(from)?;
        let recipient = address(to)?;
        ensure!(
            recipient != Pubkey::default() && sender != recipient,
            "refusing zero-address or self transfer"
        );
        self.validate_wallet(&sender)?;
        let decimals = if asset == Asset::Native { 9 } else { 6 };
        let units = amount::parse(value, decimals)?;
        let mut instructions = Vec::new();
        let mut rent = 0;
        let native_value;
        if asset == Asset::Native {
            instructions.push(system_instruction::transfer(&sender, &recipient, units));
            native_value = units;
        } else {
            native_value = 0;
            let mint = self.mint()?;
            let destination =
                spl_associated_token_account::get_associated_token_address(&recipient, &mint);
            if let Some(account) = self.account(&destination)? {
                checked_token(&account, &mint, Some(&recipient))?;
            } else {
                rent = self.rent_lamports()?;
            }
            instructions.push(spl_associated_token_account::instruction::create_associated_token_account_idempotent(&sender, &recipient, &mint, &spl_token::id()));
            let accounts = self.token_accounts(&sender, &mint)?;
            let mut remaining = units;
            for (source, account) in accounts {
                if remaining == 0 {
                    break;
                }
                let transfer = remaining.min(account.amount);
                if transfer == 0 {
                    continue;
                }
                instructions.push(spl_token::instruction::transfer_checked(
                    &spl_token::id(),
                    &source,
                    &mint,
                    &destination,
                    &sender,
                    &[],
                    transfer,
                    6,
                )?);
                remaining -= transfer;
            }
            ensure!(remaining == 0, "insufficient spendable USDT balance");
        }
        let (hash, height) = self.latest_blockhash()?;
        let message = VersionedMessage::Legacy(Message::new_with_blockhash(
            &instructions,
            Some(&sender),
            &hash,
        ));
        self.finish(
            message,
            sender,
            to.to_owned(),
            units,
            decimals,
            if asset == Asset::Native {
                "SOL"
            } else {
                "USDT"
            },
            native_value,
            rent,
            Some(height),
            now()? + 90,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        message: VersionedMessage,
        sender: Pubkey,
        to: String,
        units: u64,
        decimals: usize,
        symbol: &str,
        native_value: u64,
        rent: u64,
        height: Option<u64>,
        expires_at: u64,
    ) -> Result<Prepared> {
        validate_signer(&message, &sender)?;
        let fee = self.message_fee(&message)?;
        let required = fee
            .checked_add(rent)
            .and_then(|v| v.checked_add(native_value))
            .context("SOL budget overflow")?;
        ensure!(
            self.native_balance(&sender)? >= required,
            "insufficient SOL for amount, fee and account rent"
        );
        let unsigned = VersionedTransaction {
            signatures: vec![Signature::default()],
            message: message.clone(),
        };
        let raw = bincode::serialize(&unsigned)?;
        ensure!(
            raw.len() <= 1232,
            "transaction exceeds Solana packet size; consolidate token accounts before transferring"
        );
        self.simulate(&raw, false, None)?;
        Ok(Prepared {
            message,
            sender,
            last_valid_block_height: height,
            expires_at,
            from: sender.to_string(),
            to,
            amount: amount::format(units, decimals),
            symbol: symbol.to_owned(),
            fee: amount::format(fee, 9),
            rent: amount::format(rent, 9),
        })
    }
    fn simulate(&self, raw: &[u8], signatures: bool, accounts: Option<&[String]>) -> Result<Value> {
        let mut config = json!({"encoding":"base64", "commitment":"confirmed", "sigVerify":signatures, "replaceRecentBlockhash":false});
        if let Some(accounts) = accounts {
            config["accounts"] = json!({"encoding":"base64", "addresses":accounts});
        }
        let mut result = self.rpc("simulateTransaction", json!([STANDARD.encode(raw), config]))?;
        let value = result.get_mut("value").context("simulation has no value")?;
        ensure!(
            value.get("err").is_some(),
            "simulation lacks execution status"
        );
        ensure!(
            value["err"].is_null(),
            "Solana simulation failed: {}",
            value["err"]
        );
        Ok(value.take())
    }
    fn fresh(&self, hash: &Hash, height: Option<u64>, expires_at: u64) -> Result<()> {
        ensure!(
            now()? < expires_at,
            "transaction authorization expired; prepare again"
        );
        self.verify_network()?;
        if let Some(height) = height {
            let current = self
                .rpc("getBlockHeight", json!([{"commitment":"confirmed"}]))?
                .as_u64()
                .context("invalid block height")?;
            ensure!(
                current <= height,
                "transaction blockhash expired; prepare again"
            );
        }
        ensure!(
            self.rpc(
                "isBlockhashValid",
                json!([hash.to_string(), {"commitment":"confirmed"}])
            )?["value"]
                .as_bool()
                == Some(true),
            "transaction blockhash is no longer valid; prepare again"
        );
        Ok(())
    }
    pub fn ensure_fresh(&self, prepared: &Prepared) -> Result<()> {
        self.fresh(
            prepared.message.recent_blockhash(),
            prepared.last_valid_block_height,
            prepared.expires_at,
        )
    }
    /// Send once only; the node may propagate the exact same signed bytes, never a replacement.
    pub fn broadcast(&self, signed: &Signed) -> Result<()> {
        self.fresh(
            &signed.blockhash,
            signed.last_valid_block_height,
            signed.expires_at,
        )?;
        self.simulate(&signed.raw, true, None)?;
        (|| {
            let result = self.rpc("sendTransaction", json!([STANDARD.encode(&signed.raw), {"encoding":"base64", "skipPreflight":false, "preflightCommitment":"confirmed", "maxRetries":0}]))?;
            ensure!(result.as_str() == Some(&signed.txid), "RPC returned a different transaction signature");
            Ok(())
        })().with_context(|| format!("broadcast of {} failed or is uncertain; query this signature before sending again", signed.txid))
    }
}

pub fn sign(prepared: &Prepared, key: &[u8; 32]) -> Result<Signed> {
    ensure!(
        now()? < prepared.expires_at,
        "transaction authorization expired; prepare again"
    );
    ensure!(
        crate::core::solana_address(key)? == prepared.sender.to_string(),
        "private key does not match the prepared sender"
    );
    let signature = Signature::from(crate::core::solana_sign(
        key,
        &prepared.message.serialize(),
    )?);
    let transaction = VersionedTransaction {
        signatures: vec![signature],
        message: prepared.message.clone(),
    };
    Ok(Signed {
        txid: signature.to_string(),
        raw: bincode::serialize(&transaction)?,
        blockhash: *prepared.message.recent_blockhash(),
        last_valid_block_height: prepared.last_valid_block_height,
        expires_at: prepared.expires_at,
    })
}
fn address(input: &str) -> Result<Pubkey> {
    Pubkey::from_str(input).context("invalid Solana public key")
}
fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
fn decode_account(value: &Value) -> Result<Account> {
    ensure!(
        value["data"][1].as_str() == Some("base64"),
        "RPC account encoding is not base64"
    );
    Ok(Account {
        owner: address(value["owner"].as_str().context("account owner missing")?)?,
        lamports: value["lamports"]
            .as_u64()
            .context("account lamports missing")?,
        data: STANDARD.decode(value["data"][0].as_str().context("account data missing")?)?,
        executable: value["executable"]
            .as_bool()
            .context("account executable flag missing")?,
    })
}
fn checked_token(account: &Account, mint: &Pubkey, owner: Option<&Pubkey>) -> Result<TokenAccount> {
    ensure!(
        account.owner == spl_token::id() && !account.executable,
        "token account has the wrong program owner"
    );
    let token = TokenAccount::unpack(&account.data).context("invalid classic SPL token account")?;
    ensure!(
        token.mint == *mint && token.state == AccountState::Initialized,
        "token account has the wrong mint or is frozen/uninitialized"
    );
    if let Some(owner) = owner {
        ensure!(
            token.owner == *owner
                && token.delegate == COption::None
                && token.close_authority == COption::None,
            "token account has an unexpected owner, delegate or close authority"
        );
    }
    Ok(token)
}
fn validate_signer(message: &VersionedMessage, sender: &Pubkey) -> Result<()> {
    let header = message.header();
    ensure!(
        header.num_required_signatures == 1
            && header.num_readonly_signed_accounts == 0
            && message.static_account_keys().first() == Some(sender),
        "transaction requires an unexpected signer or fee payer"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_accounts_reject_wrong_authority_program_and_frozen_state() {
        let owner = Pubkey::new_from_array([1; 32]);
        let mint = Pubkey::new_from_array([2; 32]);
        let mut token = TokenAccount {
            mint,
            owner,
            amount: 42,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::None,
        };
        let mut account = Account {
            owner: spl_token::id(),
            lamports: 1,
            data: vec![0; TokenAccount::LEN],
            executable: false,
        };
        TokenAccount::pack(token, &mut account.data).unwrap();
        assert_eq!(
            checked_token(&account, &mint, Some(&owner)).unwrap().amount,
            42
        );
        assert!(checked_token(&account, &mint, Some(&Pubkey::new_from_array([3; 32]))).is_err());
        account.owner = system_program::id();
        assert!(checked_token(&account, &mint, Some(&owner)).is_err());
        account.owner = spl_token::id();
        token.state = AccountState::Frozen;
        TokenAccount::pack(token, &mut account.data).unwrap();
        assert!(checked_token(&account, &mint, Some(&owner)).is_err());
        token.state = AccountState::Initialized;
        token.delegate = COption::Some(Pubkey::new_from_array([3; 32]));
        TokenAccount::pack(token, &mut account.data).unwrap();
        assert!(checked_token(&account, &mint, Some(&owner)).is_err());
    }

    #[test]
    fn local_signing_preserves_message_and_rejects_wrong_key_or_expiry() {
        let key = [7; 32];
        let sender = address(&crate::core::solana_address(&key).unwrap()).unwrap();
        let recipient = Pubkey::new_from_array([3; 32]);
        let message = VersionedMessage::Legacy(Message::new_with_blockhash(
            &[system_instruction::transfer(&sender, &recipient, 42)],
            Some(&sender),
            &Hash::new_from_array([8; 32]),
        ));
        let original = message.serialize();
        let mut prepared = Prepared {
            message,
            sender,
            last_valid_block_height: Some(100),
            expires_at: now().unwrap() + 90,
            from: sender.to_string(),
            to: recipient.to_string(),
            amount: "0.000000042".to_owned(),
            symbol: "SOL".to_owned(),
            fee: "0.000005".to_owned(),
            rent: "0".to_owned(),
        };
        let signed = sign(&prepared, &key).unwrap();
        let transaction = decode_transaction(&STANDARD.encode(&signed.raw)).unwrap();
        assert_eq!(transaction.message.serialize(), original);
        assert!(transaction.signatures[0].verify(sender.as_ref(), &original));
        assert_eq!(transaction.signatures[0].to_string(), signed.txid);
        assert!(sign(&prepared, &[9; 32]).is_err());
        prepared.expires_at = 0;
        assert!(sign(&prepared, &key).is_err());
    }
}
