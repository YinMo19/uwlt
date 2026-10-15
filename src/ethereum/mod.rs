mod amount;
mod fees;
mod status;
mod swap;

pub use amount::format as format_amount;
pub use status::{TxState, TxStatus};

use std::{future::Future, time::Duration};

use alloy::{
    consensus::{SignableTransaction, TxEip1559},
    eips::eip2718::Encodable2718,
    network::TransactionBuilder,
    primitives::{Address, B256, Bytes, TxKind, U256, address},
    providers::{Provider, ProviderBuilder, RootProvider},
    rpc::types::TransactionRequest,
    signers::{SignerSync, local::PrivateKeySigner},
    sol,
    sol_types::{SolCall, SolValue},
};
use anyhow::{Context, Result, ensure};
use tokio::runtime::{Builder, Runtime};

const USDT: Address = address!("dAC17F958D2ee523a2206206994597C13D831ec7");
const POLYGON_USDT: Address = address!("c2132D05D31c914a87C6611C10748AEb04B58e8F");
const RPC_TIMEOUT: Duration = Duration::from_secs(30);

sol! {
    function balanceOf(address owner) external view returns (uint256);
    function decimals() external view returns (uint8);
    // The original Ethereum USDT contract does not return a bool.
    function transfer(address recipient, uint256 value) external;
}

#[derive(Clone, Copy, Debug)]
pub enum Network {
    Mainnet,
    Sepolia,
    Polygon,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asset {
    Usdt,
    Native,
}

pub struct Balances {
    pub usdt: Option<String>,
    pub native: String,
}

/// All signing fields are frozen before the caller requests authentication.
pub struct Prepared {
    transaction: TxEip1559,
    sender: Address,
    pub from: String,
    pub to: String,
    pub amount: String,
    pub symbol: String,
    /// Maximum total network fee in the network's native coin, not an exact charge.
    pub fee: String,
}

pub struct Signed {
    pub txid: String,
    hash: B256,
    chain_id: u64,
    raw: Vec<u8>,
}

pub struct Client {
    provider: RootProvider,
    runtime: Runtime,
    chain_id: u64,
    token: Option<Address>,
}

impl Client {
    pub fn new(network: Network, endpoint: Option<&str>, token: Option<&str>) -> Result<Self> {
        let (default_endpoint, chain_id, token) = match network {
            Network::Mainnet => ("https://ethereum-rpc.publicnode.com", 1, Some(USDT)),
            Network::Polygon => ("https://polygon.drpc.org", 137, Some(POLYGON_USDT)),
            Network::Sepolia => (
                "https://ethereum-sepolia-rpc.publicnode.com",
                11_155_111,
                token.map(parse_address).transpose()?,
            ),
        };
        let url: reqwest::Url = endpoint
            .unwrap_or(default_endpoint)
            .parse()
            .context("invalid EVM RPC URL")?;
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let provider = {
            let _guard = runtime.enter();
            ProviderBuilder::default().connect_http(url)
        };
        let client = Self {
            provider,
            runtime,
            chain_id,
            token,
        };
        client.run(async {
            let actual = client.provider.get_chain_id().await?;
            ensure!(
                actual == chain_id,
                "RPC chain ID is {actual}, expected {chain_id}"
            );
            Ok(())
        })?;
        Ok(client)
    }

    pub fn native_symbol(&self) -> &'static str {
        match self.chain_id {
            137 => "POL",
            _ => "ETH",
        }
    }

    pub fn token_symbol(&self) -> &'static str {
        match self.chain_id {
            137 => "USDT0",
            _ => "USDT",
        }
    }

    fn run<T>(&self, operation: impl Future<Output = Result<T>>) -> Result<T> {
        self.runtime.block_on(async {
            tokio::time::timeout(RPC_TIMEOUT, operation)
                .await
                .context("EVM RPC operation timed out after 30 seconds")?
        })
    }

    async fn token_balance(&self, owner: Address, token: Address) -> Result<U256> {
        if self.chain_id == 11_155_111 {
            let request = TransactionRequest::default()
                .with_to(token)
                .with_call(&decimalsCall {});
            let result = self
                .provider
                .call(request)
                .await
                .context("reading test token decimals")?;
            ensure!(
                decimalsCall::abi_decode_returns(&result)? == 6,
                "Sepolia --token must be a six-decimal USDT-compatible test contract"
            );
        }
        let request = TransactionRequest::default()
            .with_to(token)
            .with_call(&balanceOfCall { owner });
        let result = self
            .provider
            .call(request)
            .await
            .context("reading USDT balance")?;
        balanceOfCall::abi_decode_returns(&result).context("invalid USDT balance response")
    }

    pub fn balance(&self, address: &str) -> Result<Balances> {
        let address = parse_address(address)?;
        self.run(async {
            let native = self.provider.get_balance(address).await?;
            let usdt = match self.token {
                Some(token) => Some(amount::format(self.token_balance(address, token).await?, 6)),
                None => None,
            };
            Ok(Balances {
                usdt,
                native: amount::format(native, 18),
            })
        })
    }

    pub fn prepare(&self, from: &str, to: &str, amount: &str, asset: Asset) -> Result<Prepared> {
        let from = parse_address(from)?;
        let to = parse_address(to)?;
        ensure!(
            to != Address::ZERO,
            "refusing a transfer to the zero address"
        );
        let (decimals, symbol) = match asset {
            Asset::Usdt => (6, self.token_symbol()),
            Asset::Native => (18, self.native_symbol()),
        };
        let units = amount::parse(amount, decimals)?;
        self.run(async {
            let (destination, value, input) = match asset {
                Asset::Native => (to, units, Bytes::new()),
                Asset::Usdt => {
                    let token = self.token.context(
                        "Sepolia has no official USDT; configure --token with a six-decimal test contract",
                    )?;
                    ensure!(
                        self.token_balance(from, token).await? >= units,
                        "insufficient USDT balance"
                    );
                    let input = transferCall { recipient: to, value: units }.abi_encode();
                    (token, U256::ZERO, Bytes::from(input))
                }
            };
            let nonce = self.provider.get_transaction_count(from).pending().await?;
            let fees = self.current_fees().await?;
            let request = TransactionRequest::default()
                .with_from(from)
                .with_to(destination)
                .with_value(value)
                .with_input(input.clone())
                .with_chain_id(self.chain_id)
                .with_nonce(nonce)
                .with_max_fee_per_gas(fees.maximum)
                .with_max_priority_fee_per_gas(fees.priority);
            let gas_limit = self.provider.estimate_gas(request.clone()).pending().await?;
            let result = self.provider.call(request.with_gas_limit(gas_limit)).pending().await
                .context("transfer simulation failed")?;
            // Ethereum USDT returns no data; Polygon USDT0 returns a bool.
            if asset == Asset::Usdt && !result.is_empty() {
                ensure!(bool::abi_decode(&result)?, "token transfer returned false");
            }
            let maximum_fee = U256::from(gas_limit) * U256::from(fees.maximum);
            let required = value.checked_add(maximum_fee).context("amount plus fee exceeds uint256 range")?;
            ensure!(
                self.provider.get_balance(from).pending().await? >= required,
                "insufficient {} for the amount and maximum network fee", self.native_symbol()
            );
            Ok(Prepared {
                transaction: TxEip1559 {
                    chain_id: self.chain_id,
                    nonce,
                    gas_limit,
                    max_fee_per_gas: fees.maximum,
                    max_priority_fee_per_gas: fees.priority,
                    to: TxKind::Call(destination),
                    value,
                    access_list: Default::default(),
                    input,
                },
                sender: from,
                from: from.to_checksum(None),
                to: to.to_checksum(None),
                amount: amount::format(units, decimals),
                symbol: symbol.to_owned(),
                fee: amount::format(maximum_fee, 18),
            })
        })
    }

    /// Submit exactly once. An ambiguous failure must be resolved by querying txid.
    pub fn broadcast(&self, signed: &Signed) -> Result<()> {
        ensure!(
            signed.chain_id == self.chain_id,
            "signed transaction is for a different network"
        );
        self.run(async {
            let pending = self.provider.send_raw_transaction(&signed.raw).await?;
            ensure!(
                *pending.tx_hash() == signed.hash,
                "RPC returned a different transaction hash"
            );
            Ok(())
        })
        .with_context(|| {
            format!(
                "broadcast of {} failed or is uncertain; query this hash before sending again",
                signed.hash
            )
        })
    }
}

fn parse_address(input: &str) -> Result<Address> {
    let hex = input
        .strip_prefix("0x")
        .context("EVM address must start with 0x")?;
    let mixed_case = hex.bytes().any(|byte| byte.is_ascii_lowercase())
        && hex.bytes().any(|byte| byte.is_ascii_uppercase());
    if mixed_case {
        Address::parse_checksummed(input, None).context("invalid EVM address checksum")
    } else {
        input.parse().context("invalid EVM address")
    }
}

/// Pure local signing: no RPC, nonce filling, gas estimation, or field mutation.
pub fn sign(prepared: &Prepared, key: &[u8; 32]) -> Result<Signed> {
    let signer = PrivateKeySigner::from_slice(key)?;
    ensure!(
        signer.address() == prepared.sender,
        "private key does not match the prepared sender"
    );
    let signature = signer.sign_hash_sync(&prepared.transaction.signature_hash())?;
    let transaction = prepared.transaction.clone().into_signed(signature);
    let hash = *transaction.hash();
    Ok(Signed {
        txid: hash.to_string(),
        hash,
        chain_id: prepared.transaction.chain_id,
        raw: transaction.encoded_2718(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::{consensus::TxEnvelope, eips::eip2718::Decodable2718};

    #[test]
    fn signing_preserves_frozen_transaction_and_rejects_other_key() {
        let key = [1; 32];
        let sender = PrivateKeySigner::from_slice(&key).unwrap().address();
        let destination = Address::repeat_byte(2);
        let prepared = Prepared {
            transaction: TxEip1559 {
                chain_id: 11_155_111,
                nonce: u64::MAX - 1,
                gas_limit: 21_000,
                max_fee_per_gas: 2_000_000_000,
                max_priority_fee_per_gas: 1_000_000_000,
                to: TxKind::Call(destination),
                value: U256::from(1),
                access_list: Default::default(),
                input: Bytes::new(),
            },
            sender,
            from: sender.to_checksum(None),
            to: destination.to_checksum(None),
            amount: "0.000000000000000001".to_owned(),
            symbol: "ETH".to_owned(),
            fee: "0.000042".to_owned(),
        };
        assert!(sign(&prepared, &[2; 32]).is_err());
        let signed = sign(&prepared, &key).unwrap();
        let mut bytes = signed.raw.as_slice();
        let TxEnvelope::Eip1559(decoded) = TxEnvelope::decode_2718(&mut bytes).unwrap() else {
            panic!("expected an EIP-1559 transaction");
        };
        assert!(bytes.is_empty());
        assert_eq!(decoded.tx(), &prepared.transaction);
        assert_eq!(*decoded.hash(), signed.hash);
    }
}
