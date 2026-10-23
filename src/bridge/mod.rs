//! MetaMask bridge and native swap routing; transactions are signed locally.
//!
//! Trust boundary: token transfers and approvals are decoded exactly. Native
//! swaps require supported atomic router envelopes binding input/native minimum
//! and recipient; nested router semantics remain trusted. Cross-chain Solana
//! supports decoded deposits only, which do not guarantee destination delivery.
//! Uses the wallet backend's getQuote API, not a MetaMask wallet or signing service.
//! Schema: github.com/MetaMask/core/tree/main/packages/bridge-controller/src/validators.
mod amount;
mod native;
mod solana_route;
mod status;
mod transaction;

use std::time::Duration;

use alloy::primitives::{Address, U256};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{blocking, header};
use serde::de::DeserializeOwned;
use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chain {
    Tron,
    Ethereum,
    Polygon,
    Solana,
}

impl Chain {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tron => "mainnet",
            Self::Ethereum => "ethereum",
            Self::Polygon => "polygon",
            Self::Solana => "solana",
        }
    }

    pub fn native_symbol(self) -> &'static str {
        match self {
            Self::Tron => "TRX",
            Self::Ethereum => "ETH",
            Self::Polygon => "POL",
            Self::Solana => "SOL",
        }
    }

    fn chain_id(self) -> &'static str {
        match self {
            Self::Tron => "728126428",
            Self::Ethereum => "1",
            Self::Polygon => "137",
            // Numeric bridge API ID, confirmed by live /getQuote responses.
            Self::Solana => "1151111081099710",
        }
    }

    fn token(self) -> &'static str {
        match self {
            Self::Tron => crate::tron::USDT,
            Self::Ethereum => "0xdAC17F958D2ee523a2206206994597C13D831ec7",
            Self::Polygon => "0xc2132D05D31c914a87C6611C10748AEb04B58e8F",
            Self::Solana => crate::solana::USDT,
        }
    }
}

pub use native::{NativeSwapQuote, NativeSwapTransaction};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Bridge,
    Swap,
}

impl Purpose {
    pub fn stage(self) -> &'static str {
        match self {
            Self::Bridge => "bridge",
            Self::Swap => "swap",
        }
    }
}

pub struct ContractCall {
    pub purpose: Purpose,
    pub token: String,
    /// None means no new allowance instruction; never infer a spender from txTo.
    pub spender: Option<String>,
    /// Exact requested USDT input, including any input-denominated provider fee.
    pub amount: U256,
    pub to: String,
    pub data: Vec<u8>,
    pub value: U256,
    pub expires_at: Option<u64>,
}

pub enum Transaction {
    Contract(ContractCall),
    Solana(crate::solana::RemoteRoute),
}

pub struct Quote {
    pub source: Chain,
    pub destination: Chain,
    pub sender: String,
    pub recipient: String,
    pub amount: String,
    pub source_symbol: String,
    pub expected: String,
    pub minimum: String,
    pub destination_symbol: String,
    pub fees: Vec<String>,
    pub messenger: String,
    pub estimated_seconds: Option<u64>,
    pub request_id: String,
    pub bridge_id: String,
    pub slippage_bps: u16,
    pub call: Option<Transaction>,
    deadline: u64,
}

impl Quote {
    /// Check again after local authorization, immediately before broadcasting.
    pub fn ensure_fresh(&self) -> Result<()> {
        ensure!(
            now()? < self.deadline,
            "MetaMask quote expired; request a new transfer quote (nothing was sent)"
        );
        Ok(())
    }
}

pub struct Status {
    pub state: String,
    pub destination_tx: Option<String>,
    pub details: Option<String>,
}

pub struct Client {
    http: blocking::Client,
    api: &'static str,
}

impl Client {
    pub fn new() -> Result<Self> {
        // This wallet-backend endpoint currently accepts requests without an API key.
        // Identify uwlt honestly rather than impersonating the MetaMask extension.
        Ok(Self {
            http: blocking::Client::builder()
                .timeout(Duration::from_secs(45))
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .user_agent(concat!("uwlt/", env!("CARGO_PKG_VERSION")))
                .build()
                .context("initializing MetaMask HTTP client")?,
            api: "https://bridge.api.cx.metamask.io",
        })
    }

    pub fn quote(
        &self,
        source: Chain,
        destination: Chain,
        from: &str,
        to: &str,
        input: &str,
        slippage_bps: u16,
        dry_run: bool,
    ) -> Result<Quote> {
        ensure!(
            source != destination,
            "bridge requires different source and destination networks"
        );
        wallet_address(source, from)?;
        wallet_address(destination, to)?;
        ensure!(
            slippage_bps < 10_000,
            "slippage must be between 0 and 9999 basis points"
        );
        let units = amount::parse(input)?;
        let amount = units.to_string();
        let slippage = amount::slippage(slippage_bps);
        let params = [
            ("srcChainId", source.chain_id()),
            ("destChainId", destination.chain_id()),
            ("srcTokenAddress", source.token()),
            ("destTokenAddress", destination.token()),
            ("srcTokenAmount", amount.as_str()),
            ("walletAddress", from),
            ("destWalletAddress", to),
            ("slippage", slippage.as_str()),
            ("insufficientBal", if dry_run { "true" } else { "false" }),
            ("resetApproval", "false"),
            ("gasIncluded", "false"),
            ("gasIncluded7702", "false"),
        ];
        // Include request latency in the local freshness window.
        let requested_at = now()?;
        let response: Value = self.get("/getQuote", &params)?;
        select_quote(
            &response,
            source,
            destination,
            from,
            to,
            units,
            slippage_bps,
            dry_run,
            requested_at,
        )
    }

    fn get<T: DeserializeOwned>(&self, path: &str, params: &[(&str, &str)]) -> Result<T> {
        let response = self
            .http
            .get(format!("{}{path}", self.api))
            .header(header::ACCEPT, "application/json")
            .header("X-Client-Id", "uwlt")
            .header("Client-Version", env!("CARGO_PKG_VERSION"))
            .query(params)
            .send()
            // Keep addresses and amounts out of transport errors' query URLs.
            .map_err(|error| {
                anyhow::anyhow!(
                    "MetaMask {path} request failed{} (request URL omitted)",
                    if error.is_timeout() {
                        ": timed out"
                    } else {
                        ""
                    }
                )
            })?;
        let status = response.status();
        if !status.is_success() {
            let advice = match status.as_u16() {
                401 | 403 => {
                    "the MetaMask wallet backend denied uwlt access; no API key is configured by this client"
                }
                429 => "the MetaMask wallet backend rate limit was reached; try again later",
                _ => "the MetaMask wallet backend could not serve this request",
            };
            bail!("MetaMask {path}: HTTP {status}; {advice} (response body omitted)");
        }
        let body = response
            .text()
            .map_err(|_| anyhow::anyhow!("reading MetaMask {path} response failed"))?;
        serde_json::from_str(&body).map_err(|_| anyhow::anyhow!("invalid MetaMask {path} response"))
    }
}

#[allow(clippy::too_many_arguments)]
fn select_quote(
    response: &Value,
    source: Chain,
    destination: Chain,
    from: &str,
    to: &str,
    units: U256,
    slippage_bps: u16,
    dry_run: bool,
    requested_at: u64,
) -> Result<Quote> {
    let routes = response
        .as_array()
        .context("invalid MetaMask getQuote response: expected a route array")?;
    ensure!(
        !routes.is_empty(),
        "MetaMask returned no routes for this amount and network pair"
    );
    let deadline = requested_at
        .checked_add(90)
        .context("quote expiry overflow")?;
    let mut best: Option<(Quote, U256, U256)> = None;
    let mut rejections = Vec::new();
    for (index, route) in routes.iter().enumerate() {
        match parse_quote(
            route,
            source,
            destination,
            from,
            to,
            units,
            slippage_bps,
            dry_run,
            deadline,
        ) {
            Ok(candidate) => {
                let replace = best.as_ref().is_none_or(|current| {
                    (candidate.1, candidate.2) > (current.1, current.2)
                        || ((candidate.1, candidate.2) == (current.1, current.2)
                            && (&candidate.0.bridge_id, &candidate.0.request_id)
                                < (&current.0.bridge_id, &current.0.request_id))
                });
                if replace {
                    best = Some(candidate);
                }
            }
            Err(error) => rejections.push(format!("route {}: {error:#}", index + 1)),
        }
    }
    best.map(|(quote, _, _)| quote).with_context(|| {
        format!(
            "MetaMask returned no compatible routes: {}",
            rejections.join("; ")
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn parse_quote(
    response: &Value,
    source: Chain,
    destination: Chain,
    from: &str,
    to: &str,
    units: U256,
    slippage_bps: u16,
    dry_run: bool,
    mut deadline: u64,
) -> Result<(Quote, U256, U256)> {
    let route = &response["quote"];
    // Request IDs are protocol-specific (hashes and opaque strings as well as UUIDs).
    let request_id = text(route, "requestId")?;
    let bridge_id = text(route, "bridgeId")?;
    validate_chain(&route["srcChainId"], source)?;
    validate_chain(&route["destChainId"], destination)?;
    validate_token(&route["srcAsset"], source)?;
    validate_token(&route["destAsset"], destination)?;
    ensure!(
        wallet_address(source, text(route, "walletAddress")?)? == wallet_address(source, from)?,
        "MetaMask source wallet mismatch"
    );
    ensure!(
        wallet_address(destination, text(route, "destWalletAddress")?)?
            == wallet_address(destination, to)?,
        "MetaMask destination wallet mismatch"
    );
    for field in ["gasIncluded", "gasIncluded7702", "gasSponsored"] {
        ensure!(
            route[field].is_null() || route[field] == false,
            "MetaMask {field} execution is unsupported by local signing"
        );
    }
    ensure!(
        route["intent"].is_null(),
        "MetaMask route requires unsupported intent signing"
    );
    ensure!(
        route["refuel"].is_null(),
        "MetaMask route requires unsupported destination gas refueling"
    );
    let fees = quote_fees(route, source, units)?;
    let expected = amount::integer(text(route, "destTokenAmount")?)?;
    let minimum = amount::integer(text(route, "minDestTokenAmount")?)?;
    ensure!(
        !minimum.is_zero() && minimum <= expected,
        "invalid MetaMask output or minimum receive amount"
    );
    ensure!(
        minimum >= amount::minimum(expected, slippage_bps)?,
        "MetaMask minimum receive amount exceeds requested slippage ({slippage_bps} bps)"
    );
    if !route["slippage"].is_null() {
        let returned = route["slippage"]
            .as_f64()
            .context("invalid MetaMask slippage")?;
        ensure!(
            returned >= 0.0 && returned <= f64::from(slippage_bps) / 100.0,
            "MetaMask route slippage exceeds requested slippage"
        );
    }
    let bridges = route["bridges"]
        .as_array()
        .context("missing MetaMask bridges")?;
    ensure!(!bridges.is_empty(), "MetaMask route has no bridge");
    let messenger = bridges
        .iter()
        .map(|bridge| {
            let name = bridge.as_str().context("invalid MetaMask bridge name")?;
            ensure!(
                !name.trim().is_empty() && !name.chars().any(char::is_control),
                "invalid MetaMask bridge name"
            );
            Ok(name)
        })
        .collect::<Result<Vec<_>>>()?
        .join(" -> ");
    protocol_deadline(response, &mut deadline)?;
    protocol_deadline(route, &mut deadline)?;
    protocol_deadline(&response["trade"], &mut deadline)?;
    // Dry-run validates the same executable envelope without signing or broadcasting.
    let call = if source == Chain::Solana {
        Transaction::Solana(solana_route::normalize(
            response, from, to, units, deadline,
        )?)
    } else {
        Transaction::Contract(transaction::normalize(
            response, source, from, units, deadline,
        )?)
    };
    let mut fees = fees;
    if let Transaction::Contract(call) = &call {
        if !call.value.is_zero() {
            fees.push(format!(
                "{} {} | additional native value sent with trade",
                amount::format_units(call.value, if source == Chain::Tron { 6 } else { 18 }),
                source.native_symbol()
            ));
        }
    }
    fees.push(format!(
        "{} network gas is additional; estimated locally before execution",
        source.native_symbol()
    ));
    let estimated_seconds = response["estimatedProcessingTimeInSeconds"]
        .as_u64()
        .context("invalid MetaMask estimated processing time")?;
    let quote = Quote {
        source,
        destination,
        sender: from.to_owned(),
        recipient: to.to_owned(),
        amount: amount::format(units),
        source_symbol: "USDT".to_owned(),
        expected: amount::format(expected),
        minimum: amount::format(minimum),
        destination_symbol: "USDT".to_owned(),
        fees,
        messenger,
        estimated_seconds: Some(estimated_seconds),
        request_id: request_id.to_owned(),
        bridge_id: bridge_id.to_owned(),
        slippage_bps,
        call: if dry_run { None } else { Some(call) },
        deadline,
    };
    quote.ensure_fresh()?;
    Ok((quote, expected, minimum))
}

fn validate_chain(value: &Value, chain: Chain) -> Result<()> {
    ensure!(
        value.as_u64() == Some(chain.chain_id().parse::<u64>()?),
        "MetaMask chain ID mismatch"
    );
    Ok(())
}

fn validate_token(token: &Value, chain: Chain) -> Result<()> {
    validate_chain(&token["chainId"], chain)?;
    ensure!(
        wallet_address(chain, text(token, "address")?)? == wallet_address(chain, chain.token())?,
        "MetaMask token is not configured USDT"
    );
    ensure!(
        token["decimals"].as_u64() == Some(6),
        "MetaMask returned unexpected USDT decimals"
    );
    Ok(())
}

fn quote_fees(route: &Value, source: Chain, units: U256) -> Result<Vec<String>> {
    let fee_data = route["feeData"]
        .as_object()
        .context("missing MetaMask fee data")?;
    for (kind, fee) in fee_data {
        match kind.as_str() {
            "metabridge" => {}
            "txFee" => ensure!(
                amount::integer(text(fee, "amount")?)?.is_zero(),
                "MetaMask route requires unsupported gas-included token fees"
            ),
            _ => bail!("unsupported MetaMask fee category: {kind}"),
        }
    }
    let fee = &route["feeData"]["metabridge"];
    validate_token(&fee["asset"], source).context("invalid MetaMask input fee asset")?;
    let fee_units = amount::integer(text(fee, "amount")?)?;
    let routed_units = amount::integer(text(route, "srcTokenAmount")?)?;
    ensure!(
        !routed_units.is_zero() && routed_units.checked_add(fee_units) == Some(units),
        "MetaMask routed input plus metabridge fee does not equal the requested USDT amount"
    );
    // srcTokenAmount excludes this fee, but destTokenAmount is already net of it.
    // Do not add feeUsd or subtract this fee from the destination amount again.
    Ok(vec![
        format!(
            "{} USDT | MetaMask fee included in requested input",
            amount::format(fee_units)
        ),
        "Provider costs and price impact are already reflected in quoted output".to_owned(),
    ])
}

fn protocol_deadline(value: &Value, deadline: &mut u64) -> Result<()> {
    for field in ["deadline", "expiresAt", "expiry"] {
        let item = &value[field];
        if item.is_null() {
            continue;
        }
        let timestamp = if let Some(value) = item.as_u64() {
            value
        } else if let Some(value) = item.as_str() {
            if value.bytes().all(|b| b.is_ascii_digit()) {
                value.parse::<u64>().context("invalid MetaMask expiry")?
            } else {
                OffsetDateTime::parse(value, &Rfc3339)
                    .context("invalid MetaMask expiry")?
                    .unix_timestamp()
                    .try_into()
                    .context("MetaMask expiry predates Unix epoch")?
            }
        } else {
            bail!("invalid MetaMask expiry");
        };
        let timestamp = if timestamp >= 1_000_000_000_000 {
            timestamp / 1000
        } else {
            timestamp
        };
        *deadline = (*deadline).min(timestamp);
    }
    // TRON raw_data.expiration belongs to the discarded remote envelope. The local
    // executor rebuilds timestamp, expiration, block references and fee limit.
    Ok(())
}

fn now() -> Result<u64> {
    OffsetDateTime::now_utc()
        .unix_timestamp()
        .try_into()
        .context("system time predates Unix epoch")
}

fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    let text = value[field]
        .as_str()
        .with_context(|| format!("missing MetaMask {field}"))?;
    ensure!(
        !text.trim().is_empty() && !text.chars().any(char::is_control),
        "invalid MetaMask {field}"
    );
    Ok(text)
}

#[derive(Debug, PartialEq, Eq)]
enum WalletAddress {
    Account(Address),
    Solana([u8; 32]),
}

fn wallet_address(chain: Chain, input: &str) -> Result<WalletAddress> {
    if chain != Chain::Solana {
        return address(chain, input).map(WalletAddress::Account);
    }
    let mut bytes = [0; 32];
    let length = bs58::decode(input)
        .onto(&mut bytes)
        .context("invalid Solana base58 address")?;
    ensure!(
        length == 32 && bytes != [0; 32],
        "invalid Solana address length or zero address"
    );
    Ok(WalletAddress::Solana(bytes))
}

fn address(chain: Chain, input: &str) -> Result<Address> {
    ensure!(
        chain != Chain::Solana,
        "Solana addresses are not 20-byte contract addresses"
    );
    let address = if chain == Chain::Tron {
        let bytes = bs58::decode(input)
            .with_check(None)
            .into_vec()
            .context("invalid TRON Base58Check address")?;
        ensure!(
            bytes.len() == 21 && bytes[0] == 0x41,
            "invalid TRON address prefix or length"
        );
        Address::from_slice(&bytes[1..])
    } else {
        ensure!(
            input.starts_with("0x") && input.len() == 42,
            "EVM address must have 0x and 40 hexadecimal digits"
        );
        input.parse::<Address>().context("invalid EVM address")?
    };
    ensure!(
        address != Address::ZERO,
        "zero address is not valid for a bridge transfer"
    );
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FROM: &str = "0x6f33bb1763eebead07cf8815a62fcd7b30311fa3";
    const TO: &str = "TGmhZwFoCZYBcbJt82TmKQ3HXCmcV2UrGT";

    fn token(chain: Chain) -> Value {
        json!({
            "chainId": chain.chain_id().parse::<u64>().unwrap(),
            "address": chain.token(), "symbol": "USDT", "decimals": 6
        })
    }

    fn response() -> Value {
        json!({
            "quote": {
                "requestId": "tx64c-provider-specific-id", "bridgeId": "test-provider",
                "srcChainId": 1, "destChainId": 728126428,
                "srcAsset": token(Chain::Ethereum), "destAsset": token(Chain::Tron),
                "srcTokenAmount": "99125000",
                "destTokenAmount": "98177317", "minDestTokenAmount": "97686430",
                "walletAddress": FROM, "destWalletAddress": TO,
                "slippage": 0.5, "gasIncluded": false, "gasIncluded7702": false,
                "bridges": ["Test bridge"],
                "feeData": {
                    "metabridge": {
                        "asset": token(Chain::Ethereum), "amount": "875000"
                    }
                }
            },
            "trade": {
                "chainId": 1, "from": FROM, "to": Chain::Ethereum.token(),
                "value": "0x0",
                "data": format!(
                    "0xa9059cbb{:0>64}{:064x}",
                    "2222222222222222222222222222222222222222", 100_000_000u64
                )
            },
            "estimatedProcessingTimeInSeconds": 120
        })
    }

    fn select(response: &Value, dry_run: bool, requested_at: u64) -> Result<Quote> {
        select_quote(
            response,
            Chain::Ethereum,
            Chain::Tron,
            FROM,
            TO,
            U256::from(100_000_000),
            50,
            dry_run,
            requested_at,
        )
    }

    fn parse(response: Value, dry_run: bool) -> Result<Quote> {
        select(&json!([response]), dry_run, now()?)
    }

    #[test]
    fn net_input_fee_is_not_charged_or_subtracted_twice() {
        let quote = parse(response(), false).unwrap();
        assert_eq!(quote.amount, "100");
        assert_eq!(quote.expected, "98.177317");
        assert_eq!(quote.minimum, "97.68643");
        assert_eq!(
            quote.fees[0],
            "0.875 USDT | MetaMask fee included in requested input"
        );
        assert_eq!(quote.request_id, "tx64c-provider-specific-id");
        assert_eq!(quote.bridge_id, "test-provider");
        assert_eq!(quote.messenger, "Test bridge");
        let Transaction::Contract(call) = quote.call.unwrap() else {
            panic!("expected contract call")
        };
        assert_eq!(call.amount, U256::from(100_000_000));
        assert!(parse(response(), true).unwrap().call.is_none());
    }

    #[test]
    fn malformed_metadata_and_wallets_are_rejected() {
        for (field, value) in [
            ("srcChainId", json!(137)),
            ("destChainId", json!("728126428")),
            (
                "walletAddress",
                json!("0x2222222222222222222222222222222222222222"),
            ),
            ("destWalletAddress", json!(FROM)),
            ("requestId", json!("")),
            ("bridgeId", json!("bad\nid")),
            ("bridges", json!([])),
        ] {
            let mut candidate = response();
            candidate["quote"][field] = value;
            assert!(parse(candidate, true).is_err(), "{field}");
        }
        for (field, value) in [
            ("chainId", json!(137)),
            (
                "address",
                json!("0x2222222222222222222222222222222222222222"),
            ),
            ("decimals", json!(18)),
        ] {
            let mut candidate = response();
            candidate["quote"]["srcAsset"][field] = value;
            assert!(parse(candidate, true).is_err(), "{field}");
        }
        for chain in [Chain::Tron, Chain::Ethereum, Chain::Polygon, Chain::Solana] {
            validate_token(&token(chain), chain).unwrap();
        }
    }

    #[test]
    fn input_accounting_rejects_missing_extra_and_overflowing_fees() {
        for input in ["100000000", "0", "-1", "1.2", &U256::MAX.to_string()] {
            let mut candidate = response();
            candidate["quote"]["srcTokenAmount"] = json!(input);
            assert!(parse(candidate, true).is_err(), "{input}");
        }
        let mut candidate = response();
        candidate["quote"]["feeData"]["metabridge"]["asset"] = token(Chain::Polygon);
        assert!(parse(candidate, true).is_err());
        let mut candidate = response();
        candidate["quote"]["feeData"] = json!({});
        assert!(parse(candidate, true).is_err());
        let mut candidate = response();
        candidate["quote"]["feeData"]["txFee"] = json!({"amount": "1"});
        assert!(parse(candidate, true).is_err());
        let mut candidate = response();
        candidate["quote"]["feeData"]["unknown"] = json!({"amount": "1"});
        assert!(parse(candidate, true).is_err());
        let mut candidate = response();
        candidate["quote"]["srcTokenAmount"] = json!("100000000");
        candidate["quote"]["feeData"]["metabridge"]["amount"] = json!("0");
        assert!(parse(candidate, true).is_ok());
    }

    #[test]
    fn slippage_uses_provider_minimum_and_allows_only_atomic_rounding() {
        assert!(parse(response(), true).is_ok());
        for minimum in ["0", "1", "97686429", "98177318", "-1", "1.2"] {
            let mut candidate = response();
            candidate["quote"]["minDestTokenAmount"] = json!(minimum);
            assert!(parse(candidate, true).is_err(), "{minimum}");
        }
        let mut candidate = response();
        candidate["quote"]["minDestTokenAmount"] = Value::Null;
        assert!(parse(candidate, true).is_err());
        let mut candidate = response();
        candidate["quote"]["slippage"] = json!(2.0);
        assert!(parse(candidate, true).is_err());
    }

    #[test]
    fn selection_prefers_highest_compatible_output_then_minimum() {
        let mut lower = response();
        lower["quote"]["destTokenAmount"] = json!("98000000");
        lower["quote"]["minDestTokenAmount"] = json!("97600000");
        let mut unsafe_best = response();
        unsafe_best["quote"]["destTokenAmount"] = json!("100000000");
        unsafe_best["quote"]["minDestTokenAmount"] = json!("98000000");
        let mut unexecutable = response();
        unexecutable["quote"]["destTokenAmount"] = json!("100000000");
        unexecutable["quote"]["minDestTokenAmount"] = json!("99500000");
        unexecutable["trade"] = json!({"typedData": {}});
        let mut preferred = response();
        preferred["quote"]["bridgeId"] = json!("preferred");
        preferred["quote"]["minDestTokenAmount"] = json!("98000000");
        let quotes = json!([lower, unsafe_best, response(), unexecutable, preferred]);
        for dry_run in [true, false] {
            let quote = select(&quotes, dry_run, now().unwrap()).unwrap();
            assert_eq!(quote.bridge_id, "preferred");
            assert_eq!(quote.expected, "98.177317");
            assert_eq!(quote.minimum, "98");
        }
        let mut tied = response();
        tied["quote"]["bridgeId"] = json!("a-provider");
        for quotes in [json!([response(), tied]), json!([tied, response()])] {
            assert_eq!(
                select(&quotes, true, now().unwrap()).unwrap().bridge_id,
                "a-provider"
            );
        }
    }

    #[test]
    fn unavailable_routes_explain_rejections_and_never_become_fresh_again() {
        assert!(select(&json!([]), true, now().unwrap()).is_err());
        assert!(select(&json!({"error": "unavailable"}), true, now().unwrap()).is_err());
        let mut candidate = response();
        candidate["quote"]["minDestTokenAmount"] = json!("1");
        let error = parse(candidate, true).err().unwrap().to_string();
        assert!(error.contains("route 1"));
        assert!(error.contains("slippage"));
        assert!(select(&json!([response()]), true, now().unwrap() - 91).is_err());
        let mut candidate = response();
        candidate["quote"]["deadline"] = json!("2000-01-01T00:00:00Z");
        assert!(parse(candidate, true).is_err());
        let mut deadline = u64::MAX;
        protocol_deadline(&json!({"expiresAt": 1800000000000u64}), &mut deadline).unwrap();
        assert_eq!(deadline, 1_800_000_000);
    }

    #[test]
    fn alternate_execution_modes_are_not_silently_downgraded() {
        for field in ["gasIncluded", "gasIncluded7702", "gasSponsored"] {
            let mut candidate = response();
            candidate["quote"][field] = json!(true);
            assert!(parse(candidate, true).is_err(), "{field}");
        }
        for field in ["intent", "refuel"] {
            let mut candidate = response();
            candidate["quote"][field] = json!({});
            assert!(parse(candidate, true).is_err(), "{field}");
        }
    }

    #[test]
    fn solana_destination_validates_base58_without_evm_normalization() {
        let recipient = "9xQeWvG816bUx9EPfEZzxAQbBqFQvcwACV9j8CjKzG77";
        let mut candidate = response();
        candidate["quote"]["destChainId"] = json!(1_151_111_081_099_710u64);
        candidate["quote"]["destAsset"] = token(Chain::Solana);
        candidate["quote"]["destWalletAddress"] = json!(recipient);
        let select = |candidate: Value| {
            select_quote(
                &json!([candidate]),
                Chain::Ethereum,
                Chain::Solana,
                FROM,
                recipient,
                U256::from(100_000_000),
                50,
                true,
                now().unwrap(),
            )
        };
        assert!(select(candidate.clone()).is_ok());
        for bad_address in [
            recipient.to_ascii_lowercase(),
            FROM.to_owned(),
            bs58::encode([1u8; 31]).into_string(),
            bs58::encode([0u8; 32]).into_string(),
        ] {
            let mut altered = candidate.clone();
            altered["quote"]["destWalletAddress"] = json!(bad_address);
            assert!(select(altered).is_err());
        }
        let mut altered = candidate.clone();
        altered["quote"]["destAsset"]["address"] =
            json!(Chain::Solana.token().to_ascii_lowercase());
        assert!(select(altered).is_err());
        let mut altered = candidate;
        altered["quote"]["destAsset"]["decimals"] = json!(9);
        assert!(select(altered).is_err());
        assert!(address(Chain::Solana, recipient).is_err());
    }
}
