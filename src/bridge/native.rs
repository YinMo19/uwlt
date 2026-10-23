use alloy::{
    primitives::{Address, Bytes, U256, address},
    sol,
    sol_types::{SolCall, SolValue},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use super::{
    Chain, Client, ContractCall, Purpose, address, amount, now, protocol_deadline, text,
    transaction, validate_chain, validate_token, wallet_address,
};

const NATIVE: &str = "0x0000000000000000000000000000000000000000";
const SOLANA_FEE_RECIPIENT: &str = "4cLUBQKZgCv2AqGXbh8ncGhrDRcicUe3WSDzjgPY2oTA";

pub struct NativeSwapQuote {
    pub input: String,
    pub expected: String,
    pub minimum: String,
    pub route: String,
    pub fees: Vec<String>,
    pub transaction: NativeSwapTransaction,
    deadline: u64,
}

impl NativeSwapQuote {
    pub fn ensure_fresh(&self) -> Result<()> {
        ensure!(
            now()? < self.deadline,
            "MetaMask swap quote expired; request a new quote (nothing was sent)"
        );
        Ok(())
    }
}

pub enum NativeSwapTransaction {
    Contract(ContractCall),
    Solana(crate::solana::NativeSwapRoute),
}

impl Client {
    pub fn quote_swap(
        &self,
        chain: Chain,
        from: &str,
        input: &str,
        slippage_bps: u16,
        dry_run: bool,
    ) -> Result<NativeSwapQuote> {
        wallet_address(chain, from)?;
        ensure!(
            slippage_bps < 10_000,
            "slippage must be between 0 and 9999 basis points"
        );
        let units = amount::parse(input)?;
        let input = units.to_string();
        let slippage = amount::slippage(slippage_bps);
        let mut params = vec![
            ("srcChainId", chain.chain_id()),
            ("destChainId", chain.chain_id()),
            ("srcTokenAddress", chain.token()),
            ("destTokenAddress", NATIVE),
            ("srcTokenAmount", input.as_str()),
            ("walletAddress", from),
            ("destWalletAddress", from),
            ("slippage", slippage.as_str()),
            ("insufficientBal", if dry_run { "true" } else { "false" }),
            ("resetApproval", "false"),
            ("gasIncluded", "false"),
            ("gasIncluded7702", "false"),
        ];
        if chain == Chain::Solana {
            params.push(("aggIds", "jupiter"));
        }
        let requested_at = now()?;
        let response: Value = self.get("/getQuote", &params)?;
        select(&response, chain, from, units, slippage_bps, requested_at)
    }
}

fn decimals(chain: Chain) -> u8 {
    match chain {
        Chain::Tron => 6,
        Chain::Solana => 9,
        _ => 18,
    }
}

fn validate_native(asset: &Value, chain: Chain) -> Result<()> {
    validate_chain(&asset["chainId"], chain)?;
    ensure!(
        text(asset, "address")? == NATIVE,
        "MetaMask output is not the native asset"
    );
    ensure!(
        asset["decimals"].as_u64() == Some(u64::from(decimals(chain))),
        "unexpected native asset decimals"
    );
    Ok(())
}

struct Fee {
    amount: U256,
    output: bool,
    description: String,
}

fn fee(route: &Value, chain: Chain, units: U256) -> Result<Fee> {
    for (kind, fee) in route["feeData"]
        .as_object()
        .context("missing MetaMask fee data")?
    {
        match kind.as_str() {
            "metabridge" => {}
            "txFee" => ensure!(
                amount::integer(text(fee, "amount")?)?.is_zero(),
                "gas-included token fees are unsupported"
            ),
            _ => bail!("unsupported MetaMask fee category: {kind}"),
        }
    }
    let fee = &route["feeData"]["metabridge"];
    let amount = amount::integer(text(fee, "amount")?)?;
    let routed = amount::integer(text(route, "srcTokenAmount")?)?;
    ensure!(!routed.is_zero(), "MetaMask routed input must be positive");
    let output = fee["asset"]["address"].as_str() == Some(NATIVE);
    let description = if output {
        validate_native(&fee["asset"], chain)?;
        ensure!(
            routed == units,
            "output-fee route input differs from requested USDT"
        );
        format!(
            "{} {} | MetaMask fee already deducted from native output",
            amount::format_units(amount, decimals(chain)),
            chain.native_symbol()
        )
    } else {
        validate_token(&fee["asset"], chain)?;
        ensure!(
            routed.checked_add(amount) == Some(units),
            "routed input plus MetaMask fee differs from requested USDT"
        );
        format!(
            "{} USDT | MetaMask fee included in requested input",
            amount::format(amount)
        )
    };
    Ok(Fee {
        amount,
        output,
        description,
    })
}

fn select(
    response: &Value,
    chain: Chain,
    from: &str,
    units: U256,
    slippage_bps: u16,
    requested_at: u64,
) -> Result<NativeSwapQuote> {
    let routes = response
        .as_array()
        .context("invalid MetaMask getQuote response: expected a route array")?;
    let deadline = requested_at
        .checked_add(90)
        .context("quote expiry overflow")?;
    let mut best: Option<(NativeSwapQuote, U256, U256)> = None;
    let mut rejected = Vec::new();
    for (index, response) in routes.iter().enumerate() {
        match parse(response, chain, from, units, slippage_bps, deadline) {
            Ok(candidate) => {
                if best
                    .as_ref()
                    .is_none_or(|current| (candidate.1, candidate.2) > (current.1, current.2))
                {
                    best = Some(candidate);
                }
            }
            Err(error) => rejected.push(format!("route {}: {error:#}", index + 1)),
        }
    }
    best.map(|(quote, _, _)| quote).with_context(|| format!("MetaMask returned no compatible atomic native swaps (deposit/bridge routes and direct DEX fallback are not supported): {}", rejected.join("; ")))
}

fn parse(
    response: &Value,
    chain: Chain,
    from: &str,
    units: U256,
    slippage_bps: u16,
    mut deadline: u64,
) -> Result<(NativeSwapQuote, U256, U256)> {
    let route = &response["quote"];
    text(route, "requestId")?;
    let provider = text(route, "bridgeId")?;
    validate_chain(&route["srcChainId"], chain)?;
    validate_chain(&route["destChainId"], chain)?;
    validate_token(&route["srcAsset"], chain)?;
    validate_native(&route["destAsset"], chain)?;
    for field in ["walletAddress", "destWalletAddress"] {
        ensure!(
            wallet_address(chain, text(route, field)?)? == wallet_address(chain, from)?,
            "MetaMask {field} mismatch"
        );
    }
    for field in ["gasIncluded", "gasIncluded7702", "gasSponsored", "gasless"] {
        ensure!(
            route[field].is_null() || route[field] == false,
            "MetaMask {field} is unsupported by local signing"
        );
    }
    for field in ["intent", "refuel"] {
        ensure!(
            route[field].is_null(),
            "MetaMask {field} is unsupported for atomic swaps"
        );
    }
    let steps = route["steps"]
        .as_array()
        .context("missing MetaMask swap steps")?;
    for step in steps {
        ensure!(
            text(step, "action")? == "swap",
            "same-chain deposit or bridge action is not an atomic swap"
        );
        validate_chain(&step["srcChainId"], chain)?;
        validate_chain(&step["destChainId"], chain)?;
    }
    let fee = fee(route, chain, units)?;
    let expected = amount::integer(text(route, "destTokenAmount")?)?;
    let minimum = amount::integer(text(route, "minDestTokenAmount")?)?;
    ensure!(
        !minimum.is_zero() && minimum <= expected,
        "invalid MetaMask native output or minimum"
    );
    ensure!(
        minimum >= amount::minimum(expected, slippage_bps)?,
        "MetaMask minimum exceeds requested slippage"
    );
    if !route["slippage"].is_null() {
        let returned = route["slippage"]
            .as_f64()
            .context("invalid MetaMask slippage")?;
        ensure!(
            returned >= 0.0 && returned <= f64::from(slippage_bps) / 100.0,
            "MetaMask slippage exceeds request"
        );
    }
    for value in [response, route, &response["trade"]] {
        protocol_deadline(value, &mut deadline)?;
    }
    let transaction = if chain == Chain::Solana {
        ensure!(
            provider == "jupiter" && text(route, "aggregator")? == "jupiter",
            "only atomic Jupiter native swaps are supported on Solana"
        );
        ensure!(
            response["approval"].is_null() && fee.output,
            "unsupported Solana approval or input fee"
        );
        let recipient = text(&route["feeData"]["metabridge"], "recipient")?;
        ensure!(
            recipient == SOLANA_FEE_RECIPIENT,
            "unsupported MetaMask SOL fee recipient"
        );
        NativeSwapTransaction::Solana(crate::solana::NativeSwapRoute {
            transaction: text(response, "trade")?.to_owned(),
            from: from.to_owned(),
            input_amount: units.try_into().context("Solana input exceeds u64")?,
            expected: expected.try_into().context("Solana output exceeds u64")?,
            minimum: minimum.try_into().context("Solana minimum exceeds u64")?,
            fee_amount: fee.amount.try_into().context("Solana fee exceeds u64")?,
            fee_recipient: recipient.to_owned(),
            expires_at: deadline,
        })
    } else {
        let mut call = transaction::normalize(response, chain, from, units, deadline)?;
        validate_contract(&mut call, chain, from, minimum, &fee, route)?;
        call.purpose = Purpose::Swap;
        NativeSwapTransaction::Contract(call)
    };
    let quote = NativeSwapQuote {
        input: amount::format(units),
        expected: amount::format_units(expected, decimals(chain)),
        minimum: amount::format_units(minimum, decimals(chain)),
        route: format!("MetaMask / {provider}"),
        fees: vec![
            fee.description,
            "Provider costs and price impact are already reflected in quoted output".to_owned(),
            format!(
                "{} network gas is additional; budgeted locally before execution",
                chain.native_symbol()
            ),
        ],
        transaction,
        deadline,
    };
    quote.ensure_fresh()?;
    Ok((quote, expected, minimum))
}

sol! {
    function swap(string aggregatorId, address tokenFrom, uint256 amount, bytes data) external;
    struct SwapRequest {
        address requestId;
        address fromToken;
        address toToken;
        uint256 amountIn;
        uint256 platformFee;
        uint256 destinationExecutorFee;
        uint256 affiliateFee;
        address affiliatorAddress;
        uint256 minimumAmountExpected;
        bool feeFromInputToken;
        uint16 dAppTag;
        string dAppName;
    }
    struct SwapCall {
        address spender;
        address target;
        address swapFromToken;
        address swapToToken;
        bool needsTransferFromUser;
        uint256 amount;
        bytes callData;
    }
    function onChainSwaps(SwapRequest request, SwapCall[] calls, address receiver) external;
}

// Observed MetaSwap FeeDynamic adapter arguments. amountTo is a GROSS threshold;
// native fees are deducted separately. Locally strengthen it to net minimum + fee.
// MetaSwap prepends msg.sender and pays that wallet through its registered adapter:
// https://www.codeslaw.app/contracts/ethereum/0x881d40237659c251811cec9c364ef91dc08d300c
// Adapter registry / nested provider semantics remain trusted.
type AdapterArgs = (
    Address,
    Address,
    Address,
    U256,
    U256,
    Bytes,
    U256,
    Address,
    bool,
);

fn adapter_args(data: &[u8], sender: Address) -> Result<AdapterArgs> {
    // MetaSwap prepends msg.sender before delegatecalling the adapter. Dynamic
    // offsets in the provider's partial ABI already include that missing word.
    let mut complete = Vec::with_capacity(32 + data.len());
    complete.extend_from_slice(sender.into_word().as_slice());
    complete.extend_from_slice(data);
    let args =
        AdapterArgs::abi_decode_params(&complete).context("invalid MetaSwap adapter arguments")?;
    ensure!(
        args.abi_encode_params() == complete,
        "noncanonical MetaSwap adapter arguments"
    );
    Ok(args)
}

fn validate_contract(
    call: &mut ContractCall,
    chain: Chain,
    from: &str,
    minimum: U256,
    fee: &Fee,
    route: &Value,
) -> Result<()> {
    ensure!(
        call.value.is_zero(),
        "native swap must not send additional native value"
    );
    let target = address(chain, &call.to)?;
    let token = address(chain, chain.token())?;
    if let Some(spender) = &call.spender {
        ensure!(
            address(chain, spender)? == target,
            "native swap approval spender differs from router"
        );
    }
    if chain == Chain::Tron {
        ensure!(
            target == address!("f742f4589459f0923fa579600815763d1646bec3"),
            "unsupported atomic TRON router; deposits are not swaps"
        );
        let decoded = onChainSwapsCall::abi_decode(&call.data)
            .context("unsupported TRON atomic swap calldata")?;
        ensure!(
            decoded.abi_encode() == call.data,
            "noncanonical TRON atomic swap calldata"
        );
        let request = &decoded.request;
        // https://github.com/rango-exchange/rango-contracts-v2/blob/main/contracts/libraries/LibSwapper.sol
        // https://github.com/rango-exchange/rango-contracts-v2/blob/main/contracts/facets/base/RangoSwapperFacet.sol
        // onChainSwaps checks the native balance delta after fees, then sends it
        // synchronously to receiver. Inner SunSwap execution remains trusted.
        ensure!(
            decoded.receiver == address(chain, from)?
                && request.fromToken == token
                && request.toToken == Address::ZERO,
            "TRON swap wallet or asset mismatch"
        );
        ensure!(
            !fee.output
                && request.feeFromInputToken
                && request.amountIn.checked_add(fee.amount) == Some(call.amount),
            "TRON swap input fee accounting mismatch"
        );
        ensure!(
            request.platformFee.is_zero()
                && request.destinationExecutorFee.is_zero()
                && request.affiliateFee == fee.amount,
            "unsupported TRON swap fees"
        );
        ensure!(
            request.affiliatorAddress == address!("3c067dcd94cb563404b312f3114ecd307feaf531"),
            "unsupported TRON fee recipient"
        );
        ensure!(
            request.minimumAmountExpected >= minimum,
            "TRON calldata minimum is below quote"
        );
        ensure!(decoded.calls.len() == 1, "unsupported TRON swap call list");
        let inner = &decoded.calls[0];
        let sunswap = address!("4ab38f7ae7eadad03981b2a7d7883760aa63e564");
        ensure!(
            inner.target == sunswap
                && inner.spender == sunswap
                && inner.swapFromToken == token
                && inner.swapToToken == Address::ZERO
                && !inner.needsTransferFromUser
                && inner.amount == request.amountIn,
            "unsupported TRON SunSwap call envelope"
        );
        ensure!(
            inner.callData.starts_with(&[0xce, 0xf9, 0x52, 0x29]),
            "unsupported SunSwap method"
        );
    } else {
        let router = match chain {
            Chain::Ethereum => address!("881d40237659c251811cec9c364ef91dc08d300c"),
            Chain::Polygon => address!("1a1ec25dc08e98e5e93f1104b5e5cdd298707d31"),
            _ => bail!("unsupported native contract chain"),
        };
        ensure!(
            target == router,
            "unsupported MetaSwap router; deposits are not swaps"
        );
        let mut decoded =
            swapCall::abi_decode(&call.data).context("unsupported MetaSwap method")?;
        ensure!(
            decoded.abi_encode() == call.data,
            "noncanonical MetaSwap calldata"
        );
        ensure!(
            decoded.tokenFrom == token && decoded.amount == call.amount && fee.output,
            "MetaSwap input or fee asset mismatch"
        );
        ensure!(
            matches!(
                decoded.aggregatorId.as_str(),
                "uniswapPermit212FeeDynamic"
                    | "kyberSwapFeeDynamic"
                    | "0xV2"
                    | "oneInchV6FeeDynamic"
            ),
            "unsupported MetaSwap adapter; asynchronous routes are not swaps"
        );
        let mut args = adapter_args(&decoded.data, address(chain, from)?)?;
        ensure!(
            args.1 == token && args.2 == Address::ZERO && args.3 == call.amount,
            "MetaSwap adapter asset or input mismatch"
        );
        ensure!(
            args.4 >= minimum,
            "MetaSwap calldata minimum is below quote"
        );
        let recipient = match chain {
            Chain::Ethereum => address!("f326e4de8f66a0bdc0970b79e0924e33c79f1915"),
            _ => address!("930dedddb92fef1b4ab2665d250877339f064eac"),
        };
        ensure!(
            args.6 == fee.amount && args.7 == recipient && args.8,
            "MetaSwap native fee arguments mismatch"
        );
        ensure!(
            address(chain, text(&route["feeData"]["metabridge"], "recipient")?)? == recipient,
            "MetaMask native fee recipient mismatch"
        );
        ensure!(args.5.len() >= 4, "missing MetaSwap adapter call");
        let gross_minimum = minimum
            .checked_add(fee.amount)
            .context("native minimum plus fee exceeds uint256")?;
        if args.4 < gross_minimum {
            args.4 = gross_minimum;
            // Keep the encoded recipient in the offset calculation, but omit its
            // word from partial calldata: MetaSwap supplies the actual msg.sender.
            decoded.data = Bytes::from(args.abi_encode_params()).slice(32..);
            call.data = decoded.abi_encode();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(name: &str) -> Value {
        let fixtures: Value = serde_json::from_str(include_str!("native_fixtures.json")).unwrap();
        fixtures[name][0].clone()
    }

    fn parse_fixture(value: &Value, chain: Chain) -> Result<NativeSwapQuote> {
        let sender = text(&value["quote"], "walletAddress")?;
        select(
            &json!([value]),
            chain,
            sender,
            U256::from(1_000_000),
            50,
            now()?,
        )
    }

    #[test]
    fn observed_atomic_quotes_preserve_net_output_and_exact_input() {
        for (name, chain) in [
            ("ethereum", Chain::Ethereum),
            ("polygon", Chain::Polygon),
            ("tron1", Chain::Tron),
            ("solana", Chain::Solana),
        ] {
            let response = fixture(name);
            let quote = parse_fixture(&response, chain).unwrap();
            assert_eq!(quote.input, "1");
            assert_eq!(
                quote.expected,
                amount::format_units(
                    amount::integer(text(&response["quote"], "destTokenAmount").unwrap()).unwrap(),
                    decimals(chain)
                )
            );
            match quote.transaction {
                NativeSwapTransaction::Contract(call) => {
                    assert_eq!(call.amount, U256::from(1_000_000));
                    assert_eq!(call.purpose, Purpose::Swap);
                }
                NativeSwapTransaction::Solana(route) => {
                    assert_eq!(route.input_amount, 1_000_000);
                    assert_eq!(route.expected, 9_461_836);
                    assert_eq!(route.minimum, 9_414_526);
                    assert_eq!(route.fee_amount, 83_521);
                }
            }
        }
    }

    #[test]
    fn rejects_wrong_asset_chain_wallet_slippage_and_fee_accounting() {
        for (pointer, value) in [
            (
                "/quote/destAsset/address",
                json!("0x1111111111111111111111111111111111111111"),
            ),
            ("/quote/destAsset/decimals", json!(6)),
            ("/quote/destChainId", json!(137)),
            (
                "/quote/destWalletAddress",
                json!("0x1111111111111111111111111111111111111111"),
            ),
            ("/quote/srcTokenAmount", json!("999999")),
            ("/quote/minDestTokenAmount", json!("999999999999999999")),
            ("/quote/minDestTokenAmount", json!("1")),
            ("/quote/slippage", json!(1)),
            ("/quote/gasIncluded", json!(true)),
            ("/quote/feeData/metabridge/asset/chainId", json!(137)),
        ] {
            let mut response = fixture("ethereum");
            *response.pointer_mut(pointer).unwrap() = value;
            assert!(
                parse_fixture(&response, Chain::Ethereum).is_err(),
                "{pointer}"
            );
        }
        let mut tron = fixture("tron1");
        tron["quote"]["feeData"]["metabridge"]["amount"] = json!("8751");
        assert!(parse_fixture(&tron, Chain::Tron).is_err());
    }

    #[test]
    fn rejects_bridge_actions_even_when_top_level_chain_matches() {
        let mut value = fixture("tron1");
        value["quote"]["steps"][0]["action"] = json!("bridge");
        assert!(parse_fixture(&value, Chain::Tron).is_err());
        let fixtures: Value = serde_json::from_str(include_str!("native_fixtures.json")).unwrap();
        for deposit in fixtures["tron100"].as_array().unwrap() {
            let sender = text(&deposit["quote"], "walletAddress").unwrap();
            assert!(
                select(
                    &json!([deposit]),
                    Chain::Tron,
                    sender,
                    U256::from(100_000_000),
                    50,
                    now().unwrap()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn binds_evm_router_input_native_minimum_and_output_fee() {
        for change in 0..5 {
            let mut response = fixture("ethereum");
            let bytes = hex::decode(
                text(&response["trade"], "data")
                    .unwrap()
                    .trim_start_matches("0x"),
            )
            .unwrap();
            let mut call = swapCall::abi_decode(&bytes).unwrap();
            let sender = address(
                Chain::Ethereum,
                text(&response["quote"], "walletAddress").unwrap(),
            )
            .unwrap();
            let mut args = adapter_args(&call.data, sender).unwrap();
            match change {
                0 => args.2 = address!("1111111111111111111111111111111111111111"),
                1 => args.3 += U256::from(1),
                2 => args.4 = U256::from(1),
                3 => args.6 += U256::from(1),
                _ => call.aggregatorId = "relayAdapterV3".to_owned(),
            }
            call.data = args.abi_encode_params()[32..].to_vec().into();
            response["trade"]["data"] = json!(format!("0x{}", hex::encode(call.abi_encode())));
            assert!(parse_fixture(&response, Chain::Ethereum).is_err());
        }
        let mut response = fixture("ethereum");
        response["trade"]["to"] = json!("0x1111111111111111111111111111111111111111");
        assert!(parse_fixture(&response, Chain::Ethereum).is_err());
    }

    #[test]
    fn evm_gross_minimum_covers_net_receive_plus_fee_without_changing_route() {
        for (name, chain) in [("ethereum", Chain::Ethereum), ("polygon", Chain::Polygon)] {
            let response = fixture(name);
            let sender =
                address(chain, text(&response["quote"], "walletAddress").unwrap()).unwrap();
            let raw = hex::decode(
                text(&response["trade"], "data")
                    .unwrap()
                    .trim_start_matches("0x"),
            )
            .unwrap();
            let before = swapCall::abi_decode(&raw).unwrap();
            let mut expected_args = adapter_args(&before.data, sender).unwrap();
            let minimum =
                amount::integer(text(&response["quote"], "minDestTokenAmount").unwrap()).unwrap();
            expected_args.4 = expected_args.4.max(minimum + expected_args.6);
            let quote = parse_fixture(&response, chain).unwrap();
            let NativeSwapTransaction::Contract(call) = quote.transaction else {
                panic!("expected contract");
            };
            let after = swapCall::abi_decode(&call.data).unwrap();
            assert_eq!(after.aggregatorId, before.aggregatorId);
            assert_eq!(after.tokenFrom, before.tokenFrom);
            assert_eq!(after.amount, before.amount);
            assert_eq!(
                adapter_args(&after.data, sender)
                    .unwrap()
                    .abi_encode_params(),
                expected_args.abi_encode_params()
            );
            assert_eq!(after.abi_encode(), call.data);

            let mut stronger = response.clone();
            expected_args.4 += U256::from(1);
            let mut existing = before;
            existing.data = Bytes::from(expected_args.abi_encode_params()).slice(32..);
            stronger["trade"]["data"] = json!(format!("0x{}", hex::encode(existing.abi_encode())));
            let NativeSwapTransaction::Contract(preserved) =
                parse_fixture(&stronger, chain).unwrap().transaction
            else {
                panic!("expected contract");
            };
            assert_eq!(preserved.data, existing.abi_encode());
        }
    }

    #[test]
    fn binds_tron_native_recipient_minimum_and_no_extra_token_pull() {
        for change in 0..4 {
            let mut response = fixture("tron1");
            let data =
                &mut response["trade"]["raw_data"]["contract"][0]["parameter"]["value"]["data"];
            let bytes = hex::decode(data.as_str().unwrap()).unwrap();
            let mut call = onChainSwapsCall::abi_decode(&bytes).unwrap();
            match change {
                0 => call.receiver = address!("1111111111111111111111111111111111111111"),
                1 => call.request.minimumAmountExpected = U256::from(1),
                2 => call.calls[0].needsTransferFromUser = true,
                _ => call.request.destinationExecutorFee = U256::from(1),
            }
            *data = json!(hex::encode(call.abi_encode()));
            assert!(parse_fixture(&response, Chain::Tron).is_err());
        }
    }

    #[test]
    fn rejects_unsupported_solana_provider_fee_recipient_and_stale_quotes() {
        let mut response = fixture("solana");
        response["quote"]["bridgeId"] = json!("titan");
        assert!(parse_fixture(&response, Chain::Solana).is_err());
        let mut response = fixture("solana");
        response["quote"]["feeData"]["metabridge"]["recipient"] =
            response["quote"]["walletAddress"].clone();
        assert!(parse_fixture(&response, Chain::Solana).is_err());
        let response = fixture("ethereum");
        assert!(
            select(
                &json!([response]),
                Chain::Ethereum,
                text(&response["quote"], "walletAddress").unwrap(),
                U256::from(1_000_000),
                50,
                now().unwrap() - 91
            )
            .is_err()
        );
    }
}
