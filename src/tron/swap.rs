use std::time::{SystemTime, UNIX_EPOCH};

use alloy::{
    primitives::{Address, U256},
    sol,
    sol_types::{SolCall, SolEvent},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::{
    Client, Network, TxState,
    fees::{available, signed_bandwidth},
    format_amount, parse_amount,
    transaction::{
        address_hex, address_parameter, contract_success, text, transfer_success, unsigned,
        validate_call,
    },
};

// Official deployments and ABI:
// https://github.com/sun-protocol/sunswap-v2-contracts/blob/main/README.md
// https://github.com/sun-protocol/sunswap-v2-contracts/blob/main/contracts/interfaces/ISunswapV2Router01.sol
// SunSwap retains the Ethereum ABI names: WETH is WTRX and ETH outputs are TRX.
const ROUTER: &str = "TNJVzGqKBWkJxJB5XYSqGAwUTV15U24pPq";
const APPROVAL_LIMIT: u64 = 100_000_000;
const SWAP_LIMIT: u64 = 300_000_000;
const CONTRACT_LIMIT: u64 = 500_000_000;
const BANDWIDTH_CEILING: u64 = 1024;
const QUOTE_SECONDS: u64 = 20 * 60;

sol! {
    event Approval(address indexed owner, address indexed spender, uint256 value);
    function WETH() external view returns (address);
    function allowance(address owner, address spender) external view returns (uint256);
    function approve(address spender, uint256 value) external returns (bool);
    function transfer(address recipient, uint256 value) external;
    function getAmountsOut(uint256 amountIn, address[] path) external view returns (uint256[]);
    function swapExactTokensForETH(uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline) external returns (uint256[]);
}

pub struct SwapQuote {
    pub summary: crate::swap::Summary,
    plan: ContractPlan,
}

/// Fixed token allowance, calldata, native value and per-stage spending limits.
pub struct ContractPlan {
    pub fee_budget: String,
    pub approvals: usize,
    from: String,
    router: String,
    token: String,
    swap_output: Option<(u64, u64)>,
    allowance: U256,
    deadline: u64,
    bandwidth_budget: u64,
    steps: Vec<Step>,
}

struct Step {
    stage: &'static str,
    contract: String,
    data: String,
    fee_limit: u64,
    approval: Option<U256>,
    value: u64,
    transfer: Option<(String, u64)>,
}

impl Step {
    fn new<C: SolCall>(
        stage: &'static str,
        contract: &str,
        call: C,
        fee_limit: u64,
        approval: Option<U256>,
    ) -> Self {
        Self {
            stage,
            contract: contract.to_owned(),
            data: hex::encode(call.abi_encode()),
            fee_limit,
            approval,
            value: 0,
            transfer: None,
        }
    }

    fn request(&self, from: &str) -> Value {
        json!({"owner_address": from, "contract_address": self.contract,
            "data": self.data, "call_value": self.value,
            "visible": false, "fee_limit": self.fee_limit})
    }
}

impl Client {
    pub fn quote_swap(&self, from: &str, amount: &str, slippage_bps: u16) -> Result<SwapQuote> {
        self.swap_mainnet()?;
        ensure!(
            slippage_bps < 10_000,
            "slippage must be below 10000 basis points"
        );
        let amount = parse_amount(amount)?;
        ensure!(amount > 0, "swap amount must be positive");
        let from = address_hex(from)?;
        let router = address_hex(ROUTER)?;
        let owner = abi_address(&from)?;
        let wrapped = WETHCall::abi_decode_returns(&self.constant(&from, &router, WETHCall {})?)?;
        let path = vec![abi_address(&self.token)?, wrapped];
        let amounts = getAmountsOutCall::abi_decode_returns(&self.constant(
            &from,
            &router,
            getAmountsOutCall {
                amountIn: U256::from(amount),
                path: path.clone(),
            },
        )?)?;
        ensure!(
            amounts.len() == 2 && amounts[0] == U256::from(amount),
            "invalid SunSwap quote amounts"
        );
        let expected: u64 = amounts[1]
            .try_into()
            .context("TRX output exceeds supported range")?;
        let minimum = ((u128::from(expected) * u128::from(10_000 - slippage_bps)) / 10_000) as u64;
        ensure!(minimum > 0, "swap output rounds to zero");
        let allowance = self.swap_allowance(&from, &router)?;
        let deadline = now()? + QUOTE_SECONDS;
        let mut steps = self.approval_steps(&router, U256::from(amount), allowance)?;
        let approvals = steps.len();
        steps.push(Step::new(
            "swap",
            &router,
            swapExactTokensForETHCall {
                amountIn: U256::from(amount),
                amountOutMin: U256::from(minimum),
                path,
                to: owner,
                deadline: U256::from(deadline),
            },
            SWAP_LIMIT,
            None,
        ));
        let plan = self.prepare_sequence(
            from,
            router,
            allowance,
            deadline,
            steps,
            Some((amount, minimum)),
        )?;
        Ok(SwapQuote {
            summary: crate::swap::Summary {
                venue: "SunSwap V2",
                input: format_amount(amount),
                expected: format_amount(expected),
                minimum: format_amount(minimum),
                fee_budget: plan.fee_budget.clone(),
                approvals,
                token: "USDT",
                native: "TRX",
            },
            plan,
        })
    }

    pub fn prepare_contract(
        &self,
        from: &str,
        call: &crate::bridge::ContractCall,
    ) -> Result<ContractPlan> {
        ensure!(
            matches!(self.network, Network::Mainnet),
            "bridge calls require TRON mainnet"
        );
        ensure!(
            address_hex(&call.token)? == self.token,
            "bridge source token is not configured USDT"
        );
        ensure!(
            call.data.len() >= 4,
            "contract calldata has no function selector"
        );
        let from = address_hex(from)?;
        let router = address_hex(&call.spender)?;
        let destination = address_hex(&call.to)?;
        ensure!(
            abi_address(&from)? != Address::ZERO
                && abi_address(&router)? != Address::ZERO
                && abi_address(&destination)? != Address::ZERO,
            "contract sender, spender and destination must be nonzero"
        );
        let value: u64 = call
            .value
            .try_into()
            .context("TRON call value exceeds SUN range")?;
        ensure!(
            value <= i64::MAX as u64,
            "TRON call value exceeds signed protobuf range"
        );
        let transfer = if call.amount.is_zero() {
            ensure!(
                destination == self.token && router == self.token && value == 0,
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
            let amount: u64 = transfer
                .value
                .try_into()
                .context("TRON USDT deposit exceeds supported range")?;
            Some((format!("41{}", hex::encode(transfer.recipient)), amount))
        } else {
            None
        };
        let allowance = if transfer.is_some() {
            U256::ZERO
        } else {
            self.swap_allowance(&from, &router)?
        };
        let mut steps = self.approval_steps(&router, call.amount, allowance)?;
        steps.push(Step {
            stage: "bridge",
            contract: destination,
            data: hex::encode(&call.data),
            fee_limit: CONTRACT_LIMIT,
            approval: None,
            value,
            transfer,
        });
        let now = now()?;
        let deadline = call.expires_at.unwrap_or(u64::MAX).min(now + QUOTE_SECONDS);
        ensure!(
            now + 15 < deadline,
            "bridge quote expired or too close to expiry"
        );
        self.prepare_sequence(from, router, allowance, deadline, steps, None)
    }

    fn approval_steps(&self, router: &str, amount: U256, allowance: U256) -> Result<Vec<Step>> {
        let spender = abi_address(router)?;
        let mut steps = Vec::with_capacity(3);
        if allowance < amount {
            if !allowance.is_zero() {
                steps.push(Step::new(
                    "reset approval",
                    &self.token,
                    approveCall {
                        spender,
                        value: U256::ZERO,
                    },
                    APPROVAL_LIMIT,
                    Some(U256::ZERO),
                ));
            }
            steps.push(Step::new(
                "approve USDT",
                &self.token,
                approveCall {
                    spender,
                    value: amount,
                },
                APPROVAL_LIMIT,
                Some(amount),
            ));
        }
        Ok(steps)
    }

    fn prepare_sequence(
        &self,
        from: String,
        router: String,
        allowance: U256,
        deadline: u64,
        steps: Vec<Step>,
        swap_output: Option<(u64, u64)>,
    ) -> Result<ContractPlan> {
        let rates = self.fees()?;
        // Calldata may be larger than a router swap's. Reserve envelope/signature
        // overhead without asking the node to simulate before approval exists.
        let bandwidth = steps
            .iter()
            .map(|step| step.data.len() as u64 / 2 + 512)
            .max()
            .unwrap_or(BANDWIDTH_CEILING)
            .max(BANDWIDTH_CEILING);
        let bandwidth_budget = bandwidth
            .checked_mul(rates.bandwidth_sun)
            .context("bandwidth budget overflow")?;
        let total = steps
            .iter()
            .try_fold(0_u64, |total, step| {
                total
                    .checked_add(step.fee_limit)?
                    .checked_add(bandwidth_budget)?
                    .checked_add(step.value)
            })
            .context("contract fee budget overflow")?;
        Ok(ContractPlan {
            fee_budget: format_amount(total),
            approvals: steps.len() - 1,
            from,
            router,
            token: self.token.clone(),
            swap_output,
            allowance,
            deadline,
            bandwidth_budget,
            steps,
        })
    }

    pub fn execute_swap(
        &self,
        quote: &SwapQuote,
        key: &[u8; 32],
        on_transaction: impl FnMut(&str, &str),
        on_fee: impl FnMut(&str, U256),
    ) -> Result<String> {
        self.execute_contract(&quote.plan, key, on_transaction, on_fee)
    }

    pub fn execute_contract(
        &self,
        quote: &ContractPlan,
        key: &[u8; 32],
        mut on_transaction: impl FnMut(&str, &str),
        mut on_fee: impl FnMut(&str, U256),
    ) -> Result<String> {
        ensure!(
            matches!(self.network, Network::Mainnet) && self.token == quote.token,
            "contract plan belongs to a different network or token"
        );
        ensure!(
            address_hex(&crate::core::address(key)?)? == quote.from,
            "signing key does not match contract sender"
        );
        ensure!(
            now()? < quote.deadline,
            "contract plan expired; request a new quote"
        );
        ensure!(
            quote.router == quote.token
                || self.swap_allowance(&quote.from, &quote.router)? == quote.allowance,
            "USDT allowance changed since quote; request a new quote"
        );
        let mut last_txid = String::new();
        for step in &quote.steps {
            let result = (|| -> Result<String> {
                ensure!(
                    now()? + 15 < quote.deadline,
                    "contract plan expired or too close to expiry"
                );
                let request = step.request(&quote.from);
                let simulation = self.post("wallet/triggerconstantcontract", &request)?;
                contract_success(&simulation)?;
                if let Some(amount) = step.approval {
                    approval_success(&simulation, &quote.from, &quote.router, &self.token, amount)?;
                } else if let Some((recipient, amount)) = &step.transfer {
                    transfer_success(&simulation, &quote.from, recipient, &self.token, *amount)?;
                } else if let Some((amount, minimum)) = quote.swap_output {
                    let amounts = swapExactTokensForETHCall::abi_decode_returns(&constant_bytes(
                        &simulation,
                    )?)?;
                    ensure!(
                        amounts.len() == 2
                            && amounts[0] == U256::from(amount)
                            && amounts[1] >= U256::from(minimum),
                        "swap simulation violates quoted amounts"
                    );
                }
                let rates = self.fees()?;
                let energy = unsigned(&simulation, "energy_used")?;
                let resources = self.resources(&quote.from)?;
                let energy_cost = energy
                    .saturating_sub(available(&resources, "EnergyLimit", "EnergyUsed")?)
                    .checked_mul(rates.energy_sun)
                    .context("energy fee overflow")?;
                ensure!(
                    energy_cost <= step.fee_limit,
                    "simulated energy exceeds authorized fee limit; request a new quote"
                );
                // Each stage is created only after the preceding receipt, with fresh
                // block references and expiration. Calldata/minimum/deadline never change.
                let response = self.post("wallet/triggersmartcontract", &request)?;
                contract_success(&response)?;
                let transaction = response
                    .get("transaction")
                    .context("missing unsigned contract transaction")?;
                validate_call(
                    transaction,
                    &quote.from,
                    &step.contract,
                    &step.data,
                    step.fee_limit,
                    step.value,
                )?;
                let expiration = unsigned(&transaction["raw_data"], "expiration")?;
                ensure!(
                    expiration > (now()? + 10) * 1000,
                    "node returned expired or nearly expired transaction"
                );
                let bandwidth_cost = signed_bandwidth(transaction)?
                    .checked_mul(rates.bandwidth_sun)
                    .context("bandwidth fee overflow")?;
                ensure!(
                    bandwidth_cost <= quote.bandwidth_budget,
                    "transaction bandwidth exceeds frozen fee budget; request a new quote"
                );
                ensure!(
                    now()? + 5 < quote.deadline && expiration > (now()? + 5) * 1000,
                    "contract plan expired before signing; request a new quote"
                );
                let signed = crate::core::sign(key, transaction)?;
                let txid = text(&signed, "txID")?;
                on_transaction(step.stage, txid);
                ensure!(
                    now()? + 5 < quote.deadline && expiration > (now()? + 5) * 1000,
                    "transaction expired before broadcast; nothing broadcast for this stage"
                );
                self.broadcast(&signed).with_context(|| {
                    format!("{} {txid} broadcast failed or is uncertain; inspect this hash before retrying", step.stage)
                })?;
                let status = self.wait_for_receipt(txid)?;
                if let Some(fee) = status.actual_fee {
                    on_fee(step.stage, fee);
                }
                ensure!(
                    status.state == TxState::Confirmed,
                    "transaction {txid} failed: {}",
                    status
                        .message
                        .as_deref()
                        .unwrap_or("contract execution failed")
                );
                if let Some(amount) = step.approval {
                    let allowance = self
                        .swap_allowance(&quote.from, &quote.router)
                        .with_context(|| {
                            format!("checking allowance after confirmed transaction {txid}")
                        })?;
                    ensure!(
                        allowance == amount,
                        "approval receipt {txid} did not establish the requested allowance"
                    );
                }
                Ok(txid.to_owned())
            })();
            last_txid = result.with_context(|| {
                if last_txid.is_empty() {
                    format!("{} failed", step.stage)
                } else {
                    format!(
                        "{} failed; preceding confirmed transaction {last_txid}",
                        step.stage
                    )
                }
            })?;
        }
        Ok(last_txid)
    }

    fn swap_mainnet(&self) -> Result<()> {
        ensure!(
            matches!(self.network, Network::Mainnet),
            "SunSwap swaps are supported only on TRON mainnet"
        );
        Ok(())
    }

    fn constant<C: SolCall>(&self, from: &str, contract: &str, call: C) -> Result<Vec<u8>> {
        let data = call.abi_encode();
        let response = self.post("wallet/triggerconstantcontract", &json!({
            "owner_address": from, "contract_address": contract, "function_selector": C::SIGNATURE,
            "parameter": hex::encode(&data[4..]), "call_value": 0, "visible": false,
        }))?;
        contract_success(&response)?;
        constant_bytes(&response)
    }

    fn swap_allowance(&self, from: &str, router: &str) -> Result<U256> {
        allowanceCall::abi_decode_returns(&self.constant(
            from,
            &self.token,
            allowanceCall {
                owner: abi_address(from)?,
                spender: abi_address(router)?,
            },
        )?)
        .context("invalid USDT allowance response")
    }
}

fn abi_address(hex_address: &str) -> Result<Address> {
    Ok(Address::from_slice(&hex::decode(&hex_address[2..])?))
}

fn constant_bytes(response: &Value) -> Result<Vec<u8>> {
    let values = response["constant_result"]
        .as_array()
        .context("missing constant result")?;
    ensure!(values.len() == 1, "expected one constant result");
    hex::decode(values[0].as_str().context("invalid constant result")?)
        .context("invalid constant result hex")
}

fn approval_success(
    response: &Value,
    from: &str,
    router: &str,
    token: &str,
    amount: U256,
) -> Result<()> {
    // TRON USDT may return zero on success; require its actual Approval event.
    let topics = json!([
        hex::encode(Approval::SIGNATURE_HASH),
        address_parameter(from),
        address_parameter(router)
    ]);
    let data = format!("{amount:064x}");
    let logs = response["logs"]
        .as_array()
        .context("approval simulation has no logs")?;
    ensure!(
        logs.iter()
            .any(|log| log["address"].as_str() == Some(&token[2..])
                && log["topics"] == topics
                && log["data"].as_str() == Some(data.as_str())),
        "simulation did not emit the requested USDT approval"
    );
    Ok(())
}

fn now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock precedes Unix epoch")?
        .as_secs())
}
