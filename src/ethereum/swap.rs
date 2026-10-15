use alloy::eips::BlockNumberOrTag;
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

// Official deployments:
// https://docs.uniswap.org/contracts/v2/reference/smart-contracts/v2-deployments
// https://docs.quickswap.exchange/overview/contracts-and-addresses
const UNISWAP: Address = address!("7a250d5630B4cF539739dF2C5dAcb4c659F2488D");
const QUICKSWAP: Address = address!("a5E0829CaCEd8fFDD4De3c43696c57F7D7A678ff");
const APPROVAL_GAS: u64 = 120_000;
const SWAP_GAS: u64 = 400_000;
const CONTRACT_GAS: u64 = 1_000_000;
const QUOTE_LIFETIME: u64 = 20 * 60;

sol! {
    function WETH() external view returns (address);
    function getAmountsOut(uint256 amountIn, address[] path) external view returns (uint256[]);
    function allowance(address owner, address spender) external view returns (uint256);
    // Ethereum USDT returns no data; Polygon's bool is checked separately.
    function approve(address spender, uint256 value) external;
    function swapExactTokensForETH(
        uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline
    ) external returns (uint256[]);
}

/// Immutable execution parameters accompany the display-only summary. The budget
/// covers every possible stage at frozen fee caps; unused gas is not charged.
pub struct SwapQuote {
    pub summary: crate::swap::Summary,
    plan: ContractPlan,
}

/// Frozen authorization for an exact approval sequence and one contract call.
/// The budget includes native value; only actual gas consumed is charged.
pub struct ContractPlan {
    pub fee_budget: String,
    pub approvals: usize,
    chain_id: u64,
    sender: Address,
    spender: Address,
    token: Address,
    units: U256,
    balance_units: U256,
    destination: Address,
    input: Bytes,
    value: U256,
    gas_ceiling: u64,
    stage: &'static str,
    deadline: u64,
    initial_allowance: U256,
    nonce: u64,
    maximum_fee: u128,
    priority_fee: u128,
}

impl Client {
    pub fn quote_swap(&self, from: &str, input: &str, slippage_bps: u16) -> Result<SwapQuote> {
        let (router, venue) = match self.chain_id {
            1 => (UNISWAP, "Uniswap V2"),
            137 => (QUICKSWAP, "QuickSwap V2"),
            _ => anyhow::bail!(
                "USDT-to-native swaps are supported only on Ethereum and Polygon mainnet"
            ),
        };
        ensure!(
            slippage_bps < 10_000,
            "slippage must be below 10000 basis points"
        );
        let sender = parse_address(from)?;
        ensure!(
            sender != Address::ZERO,
            "swap sender cannot be the zero address"
        );
        let units = amount::parse(input, 6)?;
        let token = self.token.context("network has no USDT contract")?;
        self.run(async {
            let wrapped = self
                .provider
                .call(
                    TransactionRequest::default()
                        .with_to(router)
                        .with_call(&WETHCall {})
                        .with_gas_limit(SWAP_GAS),
                )
                .await
                .context("reading router wrapped-native token")?;
            let path = vec![token, WETHCall::abi_decode_returns(&wrapped)?];
            let result = self
                .provider
                .call(
                    TransactionRequest::default()
                        .with_to(router)
                        .with_call(&getAmountsOutCall {
                            amountIn: units,
                            path: path.clone(),
                        })
                        .with_gas_limit(SWAP_GAS),
                )
                .await
                .context("quoting the direct USDT/wrapped-native pool")?;
            let amounts = getAmountsOutCall::abi_decode_returns(&result)?;
            ensure!(
                amounts.len() == 2 && amounts[0] == units,
                "invalid router quote"
            );
            let expected = amounts[1];
            let minimum = expected
                .checked_mul(U256::from(10_000 - slippage_bps))
                .context("quote output exceeds slippage calculation range")?
                / U256::from(10_000);
            ensure!(
                minimum > U256::ZERO,
                "swap output is too small after slippage"
            );
            let deadline = unix_time()? + QUOTE_LIFETIME;
            let input = swapExactTokensForETHCall {
                amountIn: units,
                amountOutMin: minimum,
                path,
                to: sender,
                deadline: U256::from(deadline),
            }
            .abi_encode();
            let plan = self
                .prepare_sequence(
                    sender,
                    token,
                    router,
                    units,
                    router,
                    input.into(),
                    U256::ZERO,
                    SWAP_GAS,
                    "swap",
                    deadline,
                )
                .await?;
            Ok(SwapQuote {
                summary: crate::swap::Summary {
                    venue,
                    input: amount::format(units, 6),
                    expected: amount::format(expected, 18),
                    minimum: amount::format(minimum, 18),
                    fee_budget: plan.fee_budget.clone(),
                    approvals: plan.approvals,
                    token: self.token_symbol(),
                    native: self.native_symbol(),
                },
                plan,
            })
        })
    }

    pub fn prepare_contract(
        &self,
        from: &str,
        call: &crate::bridge::ContractCall,
    ) -> Result<ContractPlan> {
        ensure!(
            matches!(self.chain_id, 1 | 137),
            "bridge calls require Ethereum or Polygon mainnet"
        );
        let token = self.token.context("network has no USDT contract")?;
        ensure!(
            parse_address(&call.token)? == token,
            "bridge source token is not configured USDT"
        );
        let sender = parse_address(from)?;
        let spender = parse_address(&call.spender)?;
        let destination = parse_address(&call.to)?;
        ensure!(
            sender != Address::ZERO && spender != Address::ZERO && destination != Address::ZERO,
            "contract sender, spender and destination must be nonzero"
        );
        if call.amount.is_zero() {
            ensure!(
                destination == token && spender == token && call.value.is_zero(),
                "zero approval amount is only valid for a direct USDT deposit"
            );
            let transfer =
                transferCall::abi_decode(&call.data).context("invalid direct USDT transfer")?;
            ensure!(
                call.data == transfer.abi_encode()
                    && transfer.recipient != Address::ZERO
                    && !transfer.value.is_zero(),
                "invalid direct USDT deposit amount or recipient"
            );
        }
        ensure!(
            call.data.len() >= 4,
            "contract calldata has no function selector"
        );
        let now = unix_time()?;
        let deadline = call
            .expires_at
            .unwrap_or(u64::MAX)
            .min(now + QUOTE_LIFETIME);
        ensure!(now < deadline, "bridge quote expired; request a new quote");
        self.run(self.prepare_sequence(
            sender,
            token,
            spender,
            call.amount,
            destination,
            call.data.clone().into(),
            call.value,
            CONTRACT_GAS,
            "bridge",
            deadline,
        ))
    }

    async fn prepare_sequence(
        &self,
        sender: Address,
        token: Address,
        spender: Address,
        units: U256,
        destination: Address,
        input: Bytes,
        value: U256,
        gas_ceiling: u64,
        stage: &'static str,
        deadline: u64,
    ) -> Result<ContractPlan> {
        let initial_allowance = if units.is_zero() {
            U256::ZERO
        } else {
            self.swap_allowance(sender, token, spender).await?
        };
        let approvals = if initial_allowance >= units {
            0
        } else if initial_allowance.is_zero() {
            1
        } else {
            2
        };
        let fees = self.current_fees().await?;
        let gas_budget = gas_ceiling + APPROVAL_GAS * approvals as u64;
        let total = (U256::from(gas_budget) * U256::from(fees.maximum))
            .checked_add(value)
            .context("contract fee budget overflow")?;
        let balance_units = if units.is_zero() {
            transferCall::abi_decode(&input)?.value
        } else {
            units
        };
        Ok(ContractPlan {
            fee_budget: amount::format(total, 18),
            approvals,
            chain_id: self.chain_id,
            sender,
            spender,
            token,
            units,
            destination,
            input,
            value,
            gas_ceiling,
            stage,
            deadline,
            initial_allowance,
            balance_units,
            nonce: self
                .provider
                .get_transaction_count(sender)
                .pending()
                .await?,
            maximum_fee: fees.maximum,
            priority_fee: fees.priority,
        })
    }

    /// Preserve the quoted swap calldata, minimum and deadline through approvals.
    pub fn execute_swap(
        &self,
        quote: &SwapQuote,
        key: &[u8; 32],
        on_transaction: impl FnMut(&str, &str),
        on_fee: impl FnMut(&str, U256),
    ) -> Result<String> {
        self.execute_contract(&quote.plan, key, on_transaction, on_fee)
    }

    /// Sign and submit each stage exactly once, waiting for successful inclusion.
    pub fn execute_contract(
        &self,
        quote: &ContractPlan,
        key: &[u8; 32],
        mut on_transaction: impl FnMut(&str, &str),
        mut on_fee: impl FnMut(&str, U256),
    ) -> Result<String> {
        ensure!(
            self.chain_id == quote.chain_id && self.token == Some(quote.token),
            "contract plan belongs to a different network or token"
        );
        let signer = PrivateKeySigner::from_slice(key)?;
        ensure!(
            signer.address() == quote.sender,
            "private key does not match contract sender"
        );
        ensure!(
            unix_time()? < quote.deadline,
            "contract plan expired; request a new quote"
        );
        self.run(async {
            ensure!(
                quote.units.is_zero()
                    || self
                        .swap_allowance(quote.sender, quote.token, quote.spender)
                        .await?
                        == quote.initial_allowance,
                "spender allowance changed since the quote; request a new quote"
            );
            ensure!(
                self.token_balance(quote.sender, quote.token).await? >= quote.balance_units,
                "insufficient USDT balance"
            );
            Ok(())
        })?;
        let mut nonce = quote.nonce;
        let mut last_transaction: Option<(&str, String)> = None;
        let result = (|| {
            if quote.initial_allowance < quote.units {
                if !quote.initial_allowance.is_zero() {
                    let hash = self.swap_stage(
                        quote,
                        &signer,
                        nonce,
                        Some(U256::ZERO),
                        &mut on_transaction,
                        &mut on_fee,
                    )?;
                    last_transaction = Some(("reset approval", hash));
                    nonce = nonce.checked_add(1).context("transaction nonce overflow")?;
                }
                let hash = self.swap_stage(
                    quote,
                    &signer,
                    nonce,
                    Some(quote.units),
                    &mut on_transaction,
                    &mut on_fee,
                )?;
                last_transaction = Some(("approve", hash));
                nonce = nonce.checked_add(1).context("transaction nonce overflow")?;
            }
            self.swap_stage(
                quote,
                &signer,
                nonce,
                None,
                &mut on_transaction,
                &mut on_fee,
            )
        })();
        result.with_context(|| match last_transaction {
            Some((stage, hash)) => format!("contract sequence stopped after confirmed {stage} {hash}; that transaction is not undone"),
            None => "contract sequence failed".to_owned(),
        })
    }

    async fn swap_allowance(
        &self,
        sender: Address,
        token: Address,
        router: Address,
    ) -> Result<U256> {
        let result = self
            .provider
            .call(
                TransactionRequest::default()
                    .with_to(token)
                    .with_call(&allowanceCall {
                        owner: sender,
                        spender: router,
                    })
                    .with_gas_limit(APPROVAL_GAS),
            )
            .pending()
            .await
            .context("reading router allowance")?;
        allowanceCall::abi_decode_returns(&result).context("invalid USDT allowance response")
    }

    fn swap_stage(
        &self,
        quote: &ContractPlan,
        signer: &PrivateKeySigner,
        nonce: u64,
        approval: Option<U256>,
        on_transaction: &mut impl FnMut(&str, &str),
        on_fee: &mut impl FnMut(&str, U256),
    ) -> Result<String> {
        let (stage, destination, input, value, gas_ceiling) = match approval {
            Some(value) => (
                if value.is_zero() {
                    "reset approval"
                } else {
                    "approve"
                },
                quote.token,
                Bytes::from(
                    approveCall {
                        spender: quote.spender,
                        value,
                    }
                    .abi_encode(),
                ),
                U256::ZERO,
                APPROVAL_GAS,
            ),
            None => (
                quote.stage,
                quote.destination,
                quote.input.clone(),
                quote.value,
                quote.gas_ceiling,
            ),
        };
        let transaction = self
            .run(async {
                ensure!(
                    unix_time()? < quote.deadline,
                    "contract plan expired; request a new quote"
                );
                ensure!(
                    self.provider
                        .get_transaction_count(quote.sender)
                        .pending()
                        .await?
                        == nonce,
                    "sender nonce changed; stop and request a new quote"
                );
                let block = self
                    .provider
                    .get_block_by_number(BlockNumberOrTag::Latest)
                    .await?
                    .context("latest EVM block is unavailable")?;
                ensure!(
                    block.header.timestamp < quote.deadline,
                    "contract plan deadline passed on chain"
                );
                let base = u128::from(
                    block
                        .header
                        .base_fee_per_gas
                        .context("missing EIP-1559 base fee")?,
                );
                ensure!(
                    base.saturating_add(quote.priority_fee) <= quote.maximum_fee,
                    "current base fee exceeds the authorized fee cap; request a new quote"
                );
                let request = TransactionRequest::default()
                    .with_from(quote.sender)
                    .with_to(destination)
                    .with_input(input.clone())
                    .with_value(value)
                    .with_chain_id(self.chain_id)
                    .with_nonce(nonce)
                    .with_max_fee_per_gas(quote.maximum_fee)
                    .with_max_priority_fee_per_gas(quote.priority_fee)
                    .with_gas_limit(gas_ceiling);
                // Estimate first, then bound eth_call explicitly. Bor otherwise may
                // assume u64::MAX gas and reject a perfectly affordable transaction.
                let estimated = self
                    .provider
                    .estimate_gas(request.clone())
                    .pending()
                    .await?;
                ensure!(
                    estimated <= gas_ceiling,
                    "estimated gas exceeds the authorized stage budget"
                );
                let gas_limit = estimated.saturating_add(estimated / 5).min(gas_ceiling);
                let result = self
                    .provider
                    .call(request.with_gas_limit(gas_limit))
                    .pending()
                    .await
                    .context("transaction simulation failed")?;
                if destination == quote.token && (self.chain_id == 137 || !result.is_empty()) {
                    ensure!(bool::abi_decode(&result)?, "USDT operation returned false");
                }
                Ok(TxEip1559 {
                    chain_id: self.chain_id,
                    nonce,
                    gas_limit,
                    max_fee_per_gas: quote.maximum_fee,
                    max_priority_fee_per_gas: quote.priority_fee,
                    to: TxKind::Call(destination),
                    value,
                    access_list: Default::default(),
                    input,
                })
            })
            .with_context(|| format!("preparing {stage}"))?;
        ensure!(
            unix_time()? < quote.deadline,
            "contract plan expired before signing; request a new quote"
        );
        let signature = signer.sign_hash_sync(&transaction.signature_hash())?;
        let transaction = transaction.into_signed(signature);
        let hash = *transaction.hash();
        let signed = Signed {
            txid: hash.to_string(),
            hash,
            chain_id: self.chain_id,
            raw: transaction.encoded_2718(),
        };
        on_transaction(stage, &signed.txid);
        ensure!(
            unix_time()? < quote.deadline,
            "contract plan expired before broadcast; nothing broadcast for this stage"
        );
        self.broadcast(&signed)
            .with_context(|| format!("{stage} {} broadcast failed or is uncertain", signed.txid))?;
        let status = self.wait_for_receipt(&signed.txid).with_context(|| {
            format!(
                "{stage} {} inclusion is uncertain; inspect this hash before sending again",
                signed.txid
            )
        })?;
        if let Some(fee) = status.actual_fee {
            on_fee(stage, fee);
        }
        ensure!(
            status.state == TxState::Confirmed,
            "{stage} transaction {} reverted",
            signed.txid
        );
        if let Some(amount) = approval {
            ensure!(
                self.run(self.swap_allowance(quote.sender, quote.token, quote.spender))
                    .with_context(|| format!(
                        "checking allowance after confirmed transaction {}",
                        signed.txid
                    ))?
                    == amount,
                "approval receipt {} did not establish the requested allowance",
                signed.txid
            );
        }
        Ok(signed.txid)
    }
}

fn unix_time() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock precedes Unix epoch")?
        .as_secs())
}
