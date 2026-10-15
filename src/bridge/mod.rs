//! Native Allbridge Core transfers; no SDK runtime or remote transaction builder.
//!
//! Protocol reference: allbridge-io/allbridge-core-js-sdk commit
//! 18c19c84eb8b9fbdfb643874448daf34841bf14c, services/bridge/{utils,evm/index,trx/index}.ts
//! and services/near-intents/index.ts. API: https://api.core.allbridge.io.
//! OFT fees are deducted from the exact input, not added to its approval. NEAR
//! Intents uses a one-shot token deposit, never a bridge approval. Both still
//! require native currency for the source transaction(s).
mod amount;
mod status;

use std::{collections::HashMap, time::Duration};

use alloy::{
    primitives::{Address, B256, U256},
    sol,
    sol_types::SolCall,
};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{blocking, header};
use serde::Deserialize;
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const API: &str = "https://api.core.allbridge.io";
const OFT: u8 = 5;

sol! {
    // Current OftBridge ABI, not the deprecated liquidity-pool swapAndBridge.
    function bridge(
        address tokenAddress, uint256 amount, bytes32 recipient,
        uint256 destinationChainId, uint256 relayerFeeTokenAmount,
        uint256 extraGasInDestinationToken, uint256 slippageBP
    ) external payable;
    function transfer(address recipient, uint256 amount) external;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chain {
    Tron,
    Ethereum,
    Polygon,
}

impl Chain {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tron => "mainnet",
            Self::Ethereum => "ethereum",
            Self::Polygon => "polygon",
        }
    }

    pub fn native_symbol(self) -> &'static str {
        match self {
            Self::Tron => "TRX",
            Self::Ethereum => "ETH",
            Self::Polygon => "POL",
        }
    }

    fn api_symbol(self) -> &'static str {
        match self {
            Self::Tron => "TRX",
            Self::Ethereum => "ETH",
            Self::Polygon => "POL",
        }
    }

    fn token(self) -> &'static str {
        match self {
            Self::Tron => crate::tron::USDT,
            Self::Ethereum => "0xdAC17F958D2ee523a2206206994597C13D831ec7",
            Self::Polygon => "0xc2132D05D31c914a87C6611C10748AEb04B58e8F",
        }
    }
}

pub struct ContractCall {
    pub token: String,
    pub spender: String,
    /// Exact OFT input including deducted fees; zero means a direct token deposit
    /// with no approval (its exact debit is encoded in transfer calldata).
    pub amount: U256,
    pub to: String,
    pub data: Vec<u8>,
    pub value: U256,
    /// A one-shot deposit must be broadcast before this Unix timestamp.
    pub expires_at: Option<u64>,
}

pub struct Quote {
    pub source: Chain,
    pub destination: Chain,
    pub sender: String,
    pub recipient: String,
    pub amount: String,
    pub source_symbol: String,
    pub expected: String,
    pub destination_symbol: String,
    pub service_fee: String,
    pub messenger: String,
    pub estimated_seconds: Option<u64>,
    pub call: ContractCall,
    deadline: Option<OffsetDateTime>,
}

impl Quote {
    /// Call again after local key authorization, immediately before broadcasting.
    pub fn ensure_fresh(&self) -> Result<()> {
        if let Some(deadline) = self.deadline {
            ensure!(
                OffsetDateTime::now_utc() < deadline,
                "Allbridge deposit quote expired; request a new transfer quote (nothing was sent)"
            );
        }
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
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChainInfo {
    chain_id: u64,
    tokens: Vec<TokenInfo>,
    oft_bridge_address: Option<String>,
    #[serde(default)]
    transfer_time: HashMap<String, HashMap<String, Option<u64>>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenInfo {
    token_address: String,
    symbol: String,
    decimals: u8,
    oft_id: Option<String>,
    near_intents: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelayFee {
    fee: String,
    source_native_token_price: Option<String>,
    admin_fee_share_with_extras: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Deposit {
    deposit_address: String,
    deposit_memo: Option<String>,
    amount_in: String,
    amount_out: String,
    min_amount_out: String,
    deadline: String,
    time_estimate: u64,
}

impl Client {
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .user_agent(concat!("uwlt/", env!("CARGO_PKG_VERSION")))
                .build()
                .context("initializing Allbridge HTTP client")?,
        })
    }

    pub fn quote(
        &self,
        source: Chain,
        destination: Chain,
        from: &str,
        to: &str,
        input: &str,
    ) -> Result<Quote> {
        ensure!(
            source != destination,
            "choose different networks for a cross-chain transfer"
        );
        address(source, from).context("invalid bridge sender")?;
        let recipient = address(destination, to).context("invalid bridge recipient")?;
        let units = amount::parse(input)?;
        let metadata: HashMap<String, ChainInfo> = self.get("/token-info?filter=all")?;
        let source_info = metadata
            .get(source.api_symbol())
            .context("source network is not listed by Allbridge")?;
        let destination_info = metadata
            .get(destination.api_symbol())
            .context("destination network is not listed by Allbridge")?;
        let source_token = token(source, source_info)?;
        let destination_token = token(destination, destination_info)?;
        let mut quote = Quote {
            source,
            destination,
            sender: from.to_owned(),
            recipient: to.to_owned(),
            amount: amount::format(units),
            source_symbol: source_token.symbol.clone(),
            expected: String::new(),
            destination_symbol: destination_token.symbol.clone(),
            service_fee: String::new(),
            messenger: String::new(),
            estimated_seconds: None,
            call: ContractCall {
                expires_at: None,
                token: source_token.token_address.clone(),
                spender: String::new(),
                amount: units,
                to: String::new(),
                data: Vec::new(),
                value: U256::ZERO,
            },
            deadline: None,
        };
        let oft_supported = source_info.oft_bridge_address.is_some()
            && destination_info.oft_bridge_address.is_some()
            && source_token
                .oft_id
                .as_ref()
                .is_some_and(|id| !id.is_empty() && Some(id) == destination_token.oft_id.as_ref());
        if oft_supported {
            self.oft_quote(
                &mut quote,
                source_info,
                destination_info,
                source_token,
                recipient,
                units,
            )?;
        } else if source_token.near_intents.is_some() && destination_token.near_intents.is_some() {
            self.deposit_quote(
                &mut quote,
                source_info,
                destination_info,
                source_token,
                destination_token,
                units,
            )?;
        } else {
            bail!(
                "Allbridge currently lists no supported USDT route from {} to {}; choose another network or bridge provider",
                source.label(),
                destination.label()
            );
        }
        quote.ensure_fresh()?;
        Ok(quote)
    }

    fn oft_quote(
        &self,
        quote: &mut Quote,
        source: &ChainInfo,
        destination: &ChainInfo,
        token: &TokenInfo,
        recipient: Address,
        units: U256,
    ) -> Result<()> {
        let bridge = source
            .oft_bridge_address
            .as_deref()
            .context("missing OFT bridge")?;
        let bridge_address =
            address(quote.source, bridge).context("invalid Allbridge source contract")?;
        let token_address = address(quote.source, &token.token_address)?;
        ensure!(
            bridge_address != token_address,
            "Allbridge bridge address equals token address"
        );
        address(
            quote.destination,
            destination
                .oft_bridge_address
                .as_deref()
                .context("missing destination OFT bridge")?,
        )
        .context("invalid Allbridge destination contract")?;
        let fee: RelayFee = self.post(
            "/receive-fee",
            &json!({
                "sourceChainId": source.chain_id,
                "destinationChainId": destination.chain_id,
                "messenger": OFT,
                "sourceToken": token.token_address,
            }),
        )?;
        let stable_fee = amount::stable_fee(
            &fee.fee,
            fee.source_native_token_price
                .as_deref()
                .context("Allbridge does not offer stablecoin fee payment for this route")?,
            if quote.source == Chain::Tron { 6 } else { 18 },
        )?;
        ensure!(
            units > stable_fee,
            "amount must exceed the {} {} relay fee; increase the amount",
            amount::format(stable_fee),
            token.symbol
        );
        // The contract first deducts relay tokens, then the admin/OFT fee.
        let expected = amount::received(
            units - stable_fee,
            fee.admin_fee_share_with_extras
                .as_deref()
                .context("Allbridge did not quote an OFT admin fee")?,
        )?;
        ensure!(
            expected > U256::ZERO,
            "amount is too small after Allbridge fees"
        );
        quote.expected = amount::format(expected);
        quote.service_fee = format!(
            "~{} {} deducted from input ({} relay + ~{} protocol); source gas additional; OFT slippage limit 0.1%; no destination gas top-up",
            amount::format(units - expected),
            token.symbol,
            amount::format(stable_fee),
            amount::format(units - stable_fee - expected)
        );
        quote.messenger = "Allbridge Core / LayerZero OFT".to_owned();
        quote.estimated_seconds = source
            .transfer_time
            .get(quote.destination.api_symbol())
            .and_then(|times| times.get("oft"))
            .copied()
            .flatten()
            .map(|ms| ms.div_ceil(1000));
        quote.call.spender = bridge.to_owned();
        quote.call.to = bridge.to_owned();
        quote.call.data = bridgeCall {
            tokenAddress: token_address,
            amount: units,
            recipient: B256::from(recipient.into_word()),
            destinationChainId: U256::from(destination.chain_id),
            relayerFeeTokenAmount: stable_fee,
            extraGasInDestinationToken: U256::ZERO,
            slippageBP: U256::from(10),
        }
        .abi_encode();
        Ok(())
    }

    fn deposit_quote(
        &self,
        quote: &mut Quote,
        source: &ChainInfo,
        destination: &ChainInfo,
        source_token: &TokenInfo,
        destination_token: &TokenInfo,
        units: U256,
    ) -> Result<()> {
        // This is the SDK's buildSendTransaction flow. A deposit reservation is
        // harmless in preview: only signing/broadcasting its token transfer funds it.
        let deposit: Deposit = self.post("/near-intents/deposit", &json!({
            "sourceChainId": source.chain_id,
            "sourceToken": source_token.token_address,
            "destinationChainId": destination.chain_id,
            "destinationToken": destination_token.token_address,
            "amount": units.to_string(),
            "swapType": "EXACT_INPUT",
            "recipient": quote.recipient,
            "refundTo": quote.sender,
        })).with_context(|| format!(
            "Allbridge NEAR Intents route {} → {} unavailable; choose another network/provider or try later (Polygon USDT0 has no Allbridge OFT route)",
            quote.source.label(), quote.destination.label()
        ))?;
        ensure!(
            deposit.deposit_memo.as_deref().is_none_or(str::is_empty),
            "Allbridge returned an unsupported deposit memo"
        );
        ensure!(
            integer(&deposit.amount_in)? == units,
            "Allbridge deposit input differs from requested amount"
        );
        let expected = integer(&deposit.amount_out)?;
        let minimum = integer(&deposit.min_amount_out)?;
        ensure!(
            minimum > U256::ZERO && minimum <= expected,
            "invalid Allbridge minimum receive amount"
        );
        let deposit_address = address(quote.source, &deposit.deposit_address)
            .context("invalid Allbridge deposit address")?;
        ensure!(
            deposit_address != address(quote.source, &quote.sender)?,
            "Allbridge deposit address equals sender"
        );
        ensure!(
            deposit_address != address(quote.source, &source_token.token_address)?,
            "Allbridge deposit address equals token contract"
        );
        let deadline = OffsetDateTime::parse(&deposit.deadline, &Rfc3339)
            .context("invalid Allbridge deposit deadline")?;
        quote.deadline = Some(deadline);
        quote.expected = amount::format(expected);
        let cost = if units >= expected {
            format!(
                "~{} {} deducted from input (fees and exchange-rate impact)",
                amount::format(units - expected),
                source_token.symbol
            )
        } else {
            "included in the market-priced output (output exceeds input)".to_owned()
        };
        quote.service_fee = format!(
            "{cost}; minimum {} {}; source gas additional; deposit expires {}; refunds return to sender and may deduct fees",
            amount::format(minimum),
            destination_token.symbol,
            deposit.deadline
        );
        quote.messenger = "Allbridge Core / NEAR Intents".to_owned();
        quote.estimated_seconds = Some(deposit.time_estimate);
        quote.call.amount = U256::ZERO;
        quote.call.to = source_token.token_address.clone();
        quote.call.spender = source_token.token_address.clone();
        quote.call.data = transferCall {
            recipient: deposit_address,
            amount: units,
        }
        .abi_encode();
        quote.call.expires_at = Some(
            u64::try_from(deadline.unix_timestamp())
                .context("Allbridge deposit deadline predates Unix epoch")?,
        );
        Ok(())
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.response(
            self.http
                .get(format!("{API}{path}"))
                .header(header::ACCEPT, "application/json")
                .send()?,
            path,
        )
    }

    fn post<T: serde::de::DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T> {
        self.response(
            self.http.post(format!("{API}{path}")).json(body).send()?,
            path,
        )
    }

    fn response<T: serde::de::DeserializeOwned>(
        &self,
        response: blocking::Response,
        path: &str,
    ) -> Result<T> {
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            bail!("Allbridge {path}: HTTP {status}: {body}");
        }
        response
            .json()
            .with_context(|| format!("invalid Allbridge {path} response"))
    }
}

fn token(chain: Chain, info: &ChainInfo) -> Result<&TokenInfo> {
    let token = info
        .tokens
        .iter()
        .find(|token| {
            if chain == Chain::Tron {
                token.token_address == chain.token()
            } else {
                token.token_address.eq_ignore_ascii_case(chain.token())
            }
        })
        .with_context(|| {
            format!(
                "Allbridge does not list the wallet's canonical USDT contract on {}",
                chain.label()
            )
        })?;
    ensure!(
        token.decimals == 6,
        "Allbridge returned unexpected USDT decimals"
    );
    Ok(token)
}

fn integer(input: &str) -> Result<U256> {
    ensure!(
        !input.is_empty() && input.bytes().all(|b| b.is_ascii_digit()),
        "invalid integer token amount"
    );
    U256::from_str_radix(input, 10).context("token amount exceeds uint256")
}

/// SDK tronAddressToEthAddress strips 0x41 BEFORE left-padding to bytes32.
fn address(chain: Chain, input: &str) -> Result<Address> {
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
        "zero address is not a valid bridge recipient or contract"
    );
    Ok(address)
}
