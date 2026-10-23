mod amount;
mod fees;
mod status;
mod swap;
mod transaction;

pub use amount::{format_amount, parse_amount};
pub use status::{TxState, TxStatus};

use std::{
    cell::Cell,
    env, fmt,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use reqwest::{Url, blocking, header};
use serde_json::{Value, json};

use transaction::{
    Intent, address_hex, address_parameter, constant_word, contract_success, default_zero,
    node_message, same_hex, text, transfer_parameter, transfer_success, unsigned, validate,
};

/// Tether's official TRON mainnet USDT contract (six decimals).
pub const USDT: &str = "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asset {
    Usdt,
    Trx,
}

impl fmt::Display for Asset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Usdt => "USDT",
            Self::Trx => "TRX",
        })
    }
}

#[derive(Debug)]
pub struct Balances {
    pub usdt: u64,
    pub trx: u64,
}

#[derive(Debug)]
pub struct Prepared {
    pub transaction: Value,
    /// Simulation estimate, not a guarantee of the eventual on-chain resource cost.
    pub energy: Option<u64>,
}

pub struct Client {
    http: blocking::Client,
    endpoint: Url,
    token: String,
    next_request: Option<Cell<Instant>>,
}

impl Client {
    pub fn new(endpoint: Option<&str>) -> Result<Self> {
        let mut endpoint = Url::parse(endpoint.unwrap_or("https://api.trongrid.io/"))
            .context("invalid RPC endpoint URL")?;
        if !endpoint.path().ends_with('/') {
            endpoint.set_path(&format!("{}/", endpoint.path()));
        }
        let mut headers = header::HeaderMap::new();
        match env::var("UWLT_TRONGRID_API_KEY") {
            Ok(key) if !key.is_empty() => {
                let mut value = header::HeaderValue::from_str(&key)
                    .context("invalid UWLT_TRONGRID_API_KEY header")?;
                value.set_sensitive(true);
                headers.insert("TRON-PRO-API-KEY", value);
            }
            Ok(_) | Err(env::VarError::NotPresent) => {}
            Err(error) => return Err(error).context("invalid UWLT_TRONGRID_API_KEY"),
        }
        let public_trongrid = endpoint.host_str() == Some("api.trongrid.io")
            && !headers.contains_key("TRON-PRO-API-KEY");
        let http = blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .default_headers(headers)
            .build()
            .context("cannot initialize TRON HTTP client")?;
        Ok(Self {
            http,
            endpoint,
            token: address_hex(USDT)?,
            next_request: public_trongrid.then(|| Cell::new(Instant::now())),
        })
    }

    fn post(&self, path: &str, request: &Value) -> Result<Value> {
        // Public TronGrid allows three requests per second without an API key.
        if let Some(next) = &self.next_request {
            std::thread::sleep(next.get().saturating_duration_since(Instant::now()));
            next.set(Instant::now() + Duration::from_millis(350));
        }
        let url = self.endpoint.join(path).context("invalid RPC URL")?;
        let response = self
            .http
            .post(url)
            .json(request)
            .send()
            .with_context(|| format!("TRON RPC {path} transport failed"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .with_context(|| format!("TRON RPC {path} returned HTTP {status} with invalid JSON"))?;
        ensure!(
            status.is_success(),
            "TRON RPC {path} returned HTTP {status}: {}",
            node_message(&value)
        );
        if value.get("Error").is_some() || value.get("error").is_some() {
            bail!("TRON RPC {path}: {}", node_message(&value));
        }
        Ok(value)
    }

    pub fn balance(&self, address: &str) -> Result<Balances> {
        let address = address_hex(address)?;
        let account = self.post(
            "wallet/getaccount",
            &json!({"address": address, "visible": false}),
        )?;
        let trx = if account.as_object().is_some_and(serde_json::Map::is_empty) {
            // An unactivated account is represented by {}, not an RPC failure.
            0
        } else {
            same_hex(&account, "address", &address)?;
            default_zero(&account, "balance")?
        };
        // An unactivated account may still own TRC-20 tokens; always query the contract.
        let response = self.post(
            "wallet/triggerconstantcontract",
            &json!({
                "owner_address": address, "contract_address": self.token,
                "function_selector": "balanceOf(address)", "parameter": address_parameter(&address),
                "call_value": 0, "visible": false
            }),
        )?;
        contract_success(&response)?;
        Ok(Balances {
            trx,
            usdt: constant_word(&response)?,
        })
    }

    pub fn prepare(
        &self,
        from: &str,
        to: &str,
        amount: u64,
        asset: Asset,
        fee_limit: u64,
    ) -> Result<Prepared> {
        let from = address_hex(from)?;
        let to = address_hex(to)?;
        let (transaction, energy) = match asset {
            Asset::Trx => {
                ensure!(
                    amount <= i64::MAX as u64,
                    "TRX amount exceeds protocol int64 range"
                );
                let transaction = self.post(
                    "wallet/createtransaction",
                    &json!({
                        "owner_address": from, "to_address": to, "amount": amount, "visible": false
                    }),
                )?;
                (transaction, None)
            }
            Asset::Usdt => {
                ensure!(
                    fee_limit <= i64::MAX as u64,
                    "fee_limit exceeds protocol int64 sun"
                );
                let mut request = json!({
                    "owner_address": from, "contract_address": self.token,
                    "function_selector": "transfer(address,uint256)", "parameter": transfer_parameter(&to, amount),
                    "call_value": 0, "visible": false
                });
                let simulation = self.post("wallet/triggerconstantcontract", &request)?;
                transfer_success(&simulation, &from, &to, &self.token, amount)?;
                let energy = unsigned(&simulation, "energy_used")?;
                request["fee_limit"] = json!(fee_limit);
                let mut response = self.post("wallet/triggersmartcontract", &request)?;
                contract_success(&response)?;
                let transaction = response
                    .get_mut("transaction")
                    .context("node response missing unsigned transaction")?
                    .take();
                (transaction, Some(energy))
            }
        };
        validate(
            &transaction,
            &Intent {
                from: &from,
                to: &to,
                amount,
                asset,
                token: &self.token,
                fee_limit,
            },
        )?;
        Ok(Prepared {
            transaction,
            energy,
        })
    }

    /// Submit once. Any ambiguous failure includes the txid; query it before deciding what to do next.
    pub fn broadcast(&self, signed: &Value) -> Result<String> {
        let txid = text(signed, "txID")?;
        let result = (|| -> Result<()> {
            let response = self.post("wallet/broadcasttransaction", signed)?;
            ensure!(
                response.get("result").and_then(Value::as_bool) == Some(true),
                "broadcast rejected: {}",
                node_message(&response)
            );
            if response.get("txid").is_some() {
                same_hex(&response, "txid", txid)?;
            }
            Ok(())
        })();
        result.with_context(|| format!("broadcast not confirmed for txid {txid}; query its status before any further transfer"))?;
        Ok(txid.to_owned())
    }
}
