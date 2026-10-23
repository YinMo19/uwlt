use std::collections::HashSet;

use super::route::{is_compute, validate_ata, validate_compute};
use super::*;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_instruction::AccountMeta;

// https://developers.jup.ag/docs/swap/build
// V2 route layout: https://docs.rs/carbon-jupiter-swap-decoder/1.0.0/src/carbon_jupiter_swap_decoder/instructions/route_v2.rs.html
// The RaydiumV2 single-hop layout is also checked against a live /swap/v2/build response.
const API: &str = "https://api.jup.ag/swap/v2/build";
const JUPITER: &str = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4";
const RAYDIUM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";

pub struct SwapQuote {
    pub summary: crate::swap::Summary,
    prepared: Prepared,
}

impl Client {
    /// Single-hop Raydium AMM v4 through Jupiter V2. Router programs and RPC
    /// simulation remain trusted; neither is an independent proof of CPI effects.
    pub fn quote_swap(&self, from: &str, input: &str, slippage_bps: u16) -> Result<SwapQuote> {
        ensure!(slippage_bps < 10_000, "swap slippage must be below 100%");
        let sender = address(from)?;
        self.validate_wallet(&sender)?;
        let mint = self.mint()?;
        let native_mint = spl_token::native_mint::id();
        self.validate_mint(&native_mint, 9)?;
        let units = amount::parse(input, 6)?;
        let expires_at = now()? + 90;
        let mut request = self.http.get(API).query(&[
            ("inputMint", mint.to_string()),
            ("outputMint", native_mint.to_string()),
            ("amount", units.to_string()),
            ("taker", from.to_owned()),
            ("slippageBps", slippage_bps.to_string()),
            ("onlyDirectRoutes", "true".to_owned()),
            ("dexes", "Raydium".to_owned()),
        ]);
        match std::env::var("UWLT_JUPITER_API_KEY") {
            Ok(key) => {
                ensure!(!key.trim().is_empty(), "UWLT_JUPITER_API_KEY is empty");
                let mut value = reqwest::header::HeaderValue::from_str(&key)
                    .context("invalid UWLT_JUPITER_API_KEY header")?;
                value.set_sensitive(true);
                request = request.header("x-api-key", value);
            }
            Err(std::env::VarError::NotPresent) => {}
            Err(error) => return Err(error).context("invalid UWLT_JUPITER_API_KEY"),
        }
        let quote: Value = request
            .send()
            .context("Jupiter V2 build request failed")?
            .error_for_status()?
            .json()
            .context("invalid Jupiter V2 build response")?;
        let (expected, minimum) = validate_quote(&quote, units, slippage_bps)?;
        let pool = address(
            quote["routePlan"][0]["swapInfo"]["ammKey"]
                .as_str()
                .context("missing Raydium pool")?,
        )?;
        let source = spl_associated_token_account::get_associated_token_address(&sender, &mint);
        let destination =
            spl_associated_token_account::get_associated_token_address(&sender, &native_mint);
        let source_account = self.account(&source)?.context(
            "swap requires USDT in the sender's associated token account; same-chain transfers can aggregate other token accounts",
        )?;
        let source_balance = checked_token(&source_account, &mint, Some(&sender))?.amount;
        ensure!(
            source_balance >= units,
            "insufficient USDT in the associated token account for Jupiter swap"
        );
        let existing_destination = self.account(&destination)?;
        let previous_wrapped_lamports = if let Some(account) = &existing_destination {
            ensure!(
                checked_token(account, &native_mint, Some(&sender))?.amount == 0,
                "empty the existing wrapped SOL account before swapping to native SOL"
            );
            account.lamports
        } else {
            0
        };
        let mut instructions = build_instructions(&quote, &sender)?;
        let mut rent = 0u64;
        let mut budgets = HashSet::new();
        let mut created = false;
        let mut swapped = false;
        let mut closed = false;
        for instruction in &mut instructions {
            if is_compute(instruction) {
                ensure!(!created && !swapped, "compute instruction after swap setup");
                validate_compute(instruction, &mut budgets)?;
            } else if instruction.program_id == spl_associated_token_account::id() {
                ensure!(!created && !swapped, "unexpected extra token account setup");
                let (ata, owner, account_mint) = validate_ata(instruction, &sender)?;
                ensure!(
                    ata == destination && owner == sender && account_mint == native_mint,
                    "swap creates an unapproved token account"
                );
                if existing_destination.is_none() {
                    rent = self.rent_lamports()?;
                }
                created = true;
            } else if instruction.program_id == address(JUPITER)? {
                ensure!(
                    !swapped && !closed,
                    "multiple or reordered swap instructions"
                );
                validate_jupiter_route(
                    instruction,
                    sender,
                    source,
                    destination,
                    pool,
                    units,
                    expected,
                    slippage_bps,
                )?;
                let pool_account = self
                    .account(&pool)?
                    .context("Raydium pool does not exist")?;
                ensure!(
                    pool_account.owner == address(RAYDIUM)? && !pool_account.executable,
                    "pool is not owned by the official Raydium AMM program"
                );
                let authority = instruction.accounts[13].pubkey;
                let mut vault_mints = HashSet::new();
                for index in [14, 15] {
                    let account = self
                        .account(&instruction.accounts[index].pubkey)?
                        .context("Raydium vault missing")?;
                    ensure!(
                        account.owner == spl_token::id() && !account.executable,
                        "invalid Raydium vault program"
                    );
                    let token = TokenAccount::unpack(&account.data)?;
                    ensure!(
                        token.owner == authority
                            && token.owner != sender
                            && [mint, native_mint].contains(&token.mint)
                            && vault_mints.insert(token.mint),
                        "Raydium vault mint or authority mismatch"
                    );
                }
                // Router instructions are ours to compose. Encode the displayed
                // minimum directly with zero further slippage, avoiding a second
                // rounding step in the on-chain program. No fees or surplus share.
                instruction.data[16..24].copy_from_slice(&minimum.to_le_bytes());
                instruction.data[24..26].copy_from_slice(&0u16.to_le_bytes());
                swapped = true;
            } else if instruction.program_id == spl_token::id() {
                ensure!(
                    swapped
                        && !closed
                        && instruction.data == [9]
                        && instruction.accounts.len() == 3,
                    "unsupported swap token instruction"
                );
                ensure!(
                    instruction.accounts[0].pubkey == destination
                        && instruction.accounts[1].pubkey == sender
                        && instruction.accounts[2].pubkey == sender
                        && instruction.accounts[2].is_signer,
                    "swap must unwrap SOL to the sender"
                );
                closed = true;
            } else {
                bail!(
                    "unsupported Jupiter program {}; refusing blind signing",
                    instruction.program_id
                );
            }
        }
        ensure!(
            swapped && closed,
            "swap must include one validated swap and native SOL unwrapping"
        );
        if !budgets.contains(&2) {
            // A bounded budget for this single-hop layout; simulation must succeed.
            instructions.insert(0, ComputeBudgetInstruction::set_compute_unit_limit(200_000));
        }
        let (hash, height) = self.latest_blockhash()?;
        // Single-hop Raydium fits a legacy message. Do not trust the provider's
        // lookup-table contents or blockhash; compile the checked public accounts locally.
        let message = VersionedMessage::Legacy(Message::new_with_blockhash(
            &instructions,
            Some(&sender),
            &hash,
        ));
        validate_signer(&message, &sender)?;
        let fee = self.message_fee(&message)?;
        let before_sol = self.native_balance(&sender)?;
        let fee_budget = fee.checked_add(rent).context("swap fee budget overflow")?;
        ensure!(
            before_sol >= fee_budget,
            "insufficient SOL for swap fee and account rent"
        );
        let unsigned = VersionedTransaction {
            signatures: vec![Signature::default()],
            message,
        };
        let raw = bincode::serialize(&unsigned)?;
        ensure!(
            raw.len() <= 1232,
            "Jupiter route exceeds Solana packet size"
        );
        let accounts = [
            sender.to_string(),
            source.to_string(),
            destination.to_string(),
        ];
        let simulated = self.simulate(&raw, false, Some(&accounts))?;
        let after = simulated["accounts"]
            .as_array()
            .context("swap simulation did not return account state")?;
        ensure!(
            after.len() == 3,
            "swap simulation has incomplete account state"
        );
        let after_sender = decode_account(&after[0])?;
        ensure!(
            after_sender.owner == system_program::id()
                && after_sender.data.is_empty()
                && !after_sender.executable,
            "swap changes wallet ownership"
        );
        let after_source = checked_token(&decode_account(&after[1])?, &mint, Some(&sender))?;
        ensure!(
            source_balance.checked_sub(after_source.amount) == Some(units),
            "simulation does not spend the exact approved USDT input"
        );
        ensure!(
            after[2].is_null(),
            "simulation leaves wrapped SOL instead of native SOL"
        );
        ensure!(
            u128::from(after_sender.lamports) + u128::from(fee)
                >= u128::from(before_sol)
                    + u128::from(minimum)
                    + u128::from(previous_wrapped_lamports),
            "simulated native SOL output is below the approved minimum"
        );
        self.fresh(&hash, Some(height), expires_at)?;
        let prepared = Prepared {
            message: unsigned.message,
            sender,
            last_valid_block_height: Some(height),
            expires_at,
            from: sender.to_string(),
            to: sender.to_string(),
            amount: amount::format(units, 6),
            symbol: "USDT".to_owned(),
            fee: amount::format(fee, 9),
            rent: amount::format(rent, 9),
        };
        Ok(SwapQuote {
            summary: crate::swap::Summary {
                venue: "Jupiter V2 / Raydium AMM v4",
                input: amount::format(units, 6),
                expected: amount::format(expected, 9),
                minimum: amount::format(minimum, 9),
                fee_budget: amount::format(fee_budget, 9),
                approvals: 0,
                token: "USDT",
                native: "SOL",
            },
            prepared,
        })
    }

    pub fn execute_swap(
        &self,
        quote: &SwapQuote,
        key: &[u8; 32],
        mut on_transaction: impl FnMut(&str, &str),
        mut on_fee: impl FnMut(&str, u64),
    ) -> Result<String> {
        self.ensure_fresh(&quote.prepared)?;
        let signed = sign(&quote.prepared, key)?;
        on_transaction("swap", &signed.txid);
        self.broadcast(&signed)?;
        let receipt = self.wait_for_receipt(&signed.txid)?;
        if let Some(fee) = receipt.actual_fee {
            on_fee("swap", fee);
        }
        ensure!(
            receipt.state == TxState::Confirmed,
            "swap {} failed: {}",
            signed.txid,
            receipt
                .error
                .as_deref()
                .unwrap_or("unknown execution error")
        );
        Ok(signed.txid)
    }
}

fn instruction(value: &Value, sender: &Pubkey) -> Result<Instruction> {
    let accounts = value["accounts"]
        .as_array()
        .context("instruction accounts missing")?;
    let accounts = accounts
        .iter()
        .map(|account| {
            let pubkey = address(
                account["pubkey"]
                    .as_str()
                    .context("instruction account key missing")?,
            )?;
            let is_signer = account["isSigner"]
                .as_bool()
                .context("account signer flag missing")?;
            ensure!(
                !is_signer || pubkey == *sender,
                "Jupiter requests another signer"
            );
            Ok(AccountMeta {
                pubkey,
                is_signer,
                is_writable: account["isWritable"]
                    .as_bool()
                    .context("account writable flag missing")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Instruction {
        program_id: address(
            value["programId"]
                .as_str()
                .context("instruction program missing")?,
        )?,
        accounts,
        data: STANDARD.decode(value["data"].as_str().context("instruction data missing")?)?,
    })
}

fn build_instructions(quote: &Value, sender: &Pubkey) -> Result<Vec<Instruction>> {
    ensure!(quote["tipInstruction"].is_null(), "unapproved Jupiter tip");
    ensure!(
        quote["otherInstructions"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "unsupported extra Jupiter instructions"
    );
    let compute = quote["computeBudgetInstructions"]
        .as_array()
        .context("compute instructions missing")?;
    let setup = quote["setupInstructions"]
        .as_array()
        .context("setup instructions missing")?;
    ensure!(
        compute.len() <= 2 && setup.len() <= 1,
        "unsupported Jupiter setup layout"
    );
    let mut instructions = Vec::with_capacity(compute.len() + setup.len() + 2);
    for value in compute.iter().chain(setup) {
        instructions.push(instruction(value, sender)?);
    }
    instructions.push(instruction(&quote["swapInstruction"], sender)?);
    instructions.push(instruction(&quote["cleanupInstruction"], sender)?);
    Ok(instructions)
}

fn quote_amount(quote: &Value, field: &str) -> Result<u64> {
    quote[field]
        .as_str()
        .with_context(|| format!("missing quote {field}"))?
        .parse()
        .with_context(|| format!("invalid quote {field}"))
}

fn validate_quote(quote: &Value, input: u64, slippage: u16) -> Result<(u64, u64)> {
    ensure!(slippage < 10_000, "invalid swap slippage");
    ensure!(
        quote["inputMint"].as_str() == Some(USDT)
            && quote["outputMint"].as_str() == Some(&spl_token::native_mint::id().to_string())
            && quote_amount(quote, "inAmount")? == input
            && quote["swapMode"].as_str() == Some("ExactIn")
            && quote["slippageBps"].as_u64() == Some(u64::from(slippage)),
        "Jupiter quote does not match the requested swap"
    );
    ensure!(
        quote["platformFee"].is_null() || quote["platformFee"]["feeBps"].as_u64() == Some(0),
        "unapproved Jupiter platform fee"
    );
    let plan = quote["routePlan"]
        .as_array()
        .context("missing Jupiter route plan")?;
    ensure!(
        plan.len() == 1
            && plan[0]["bps"].as_u64() == Some(10_000)
            && plan[0]["swapInfo"]["label"].as_str() == Some("Raydium"),
        "only direct Raydium AMM v4 routes are supported"
    );
    let expected = quote_amount(quote, "outAmount")?;
    let minimum = quote_amount(quote, "otherAmountThreshold")?;
    let floor =
        u64::try_from((u128::from(expected) * u128::from(10_000 - slippage)).div_ceil(10_000))?;
    ensure!(
        expected > 0 && minimum > 0 && minimum >= floor && minimum <= expected,
        "Jupiter minimum exceeds requested slippage"
    );
    Ok((expected, minimum))
}

#[allow(clippy::too_many_arguments)]
fn validate_jupiter_route(
    ix: &Instruction,
    sender: Pubkey,
    source: Pubkey,
    destination: Pubkey,
    pool: Pubkey,
    input: u64,
    expected: u64,
    slippage: u16,
) -> Result<()> {
    let data = &ix.data;
    let discriminator = solana_sha256_hasher::hash(b"global:route_v2").to_bytes();
    ensure!(
        data.len() == 39 && data[..8] == discriminator[..8],
        "unsupported Jupiter instruction; expected route_v2"
    );
    ensure!(
        u64::from_le_bytes(data[8..16].try_into()?) == input
            && u64::from_le_bytes(data[16..24].try_into()?) == expected
            && u16::from_le_bytes(data[24..26].try_into()?) == slippage
            && data[26..30] == [0; 4],
        "Jupiter changes input, output, slippage or fees"
    );
    ensure!(
        data[30..34] == 1u32.to_le_bytes() && data[34..39] == [105, 16, 39, 0, 1],
        "only one full-weight RaydiumV2 route step is supported"
    );
    let a = &ix.accounts;
    let jupiter = address(JUPITER)?;
    ensure!(
        a.len() == 19
            && a[0].pubkey == sender
            && a[0].is_signer
            && a[1].pubkey == source
            && a[1].is_writable
            && a[2].pubkey == destination
            && a[2].is_writable
            && a[3].pubkey == address(USDT)?
            && a[4].pubkey == spl_token::native_mint::id()
            && a[5].pubkey == spl_token::id()
            && a[6].pubkey == spl_token::id()
            && a[7].pubkey == jupiter
            && a[8].pubkey == Pubkey::find_program_address(&[b"__event_authority"], &jupiter).0
            && a[9].pubkey == jupiter
            && a[10].pubkey == address(RAYDIUM)?
            && a[11].pubkey == spl_token::id()
            && a[12].pubkey == pool
            && a[16].pubkey == source
            && a[17].pubkey == destination
            && a[18].pubkey == sender,
        "Jupiter route changes programs, pool, mints, recipient or authority"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_enforces_rounded_minimum_and_exact_input() {
        // Non-divisible amounts from the live V2 build response; thresholds round up.
        let mut quote = json!({"inputMint":USDT,"outputMint":spl_token::native_mint::id().to_string(),
            "inAmount":"10000000","outAmount":"95562514","otherAmountThreshold":"95084702",
            "slippageBps":50,"swapMode":"ExactIn","routePlan":[{"bps":10000,"swapInfo":{"label":"Raydium"}}]});
        assert_eq!(
            validate_quote(&quote, 10_000_000, 50).unwrap(),
            (95_562_514, 95_084_702)
        );
        assert!(validate_quote(&quote, 10_000_001, 50).is_err());
        assert!(validate_quote(&quote, 10_000_000, 51).is_err());
        quote["otherAmountThreshold"] = json!("95084701");
        assert!(validate_quote(&quote, 10_000_000, 50).is_err());
        quote["otherAmountThreshold"] = json!("95562514");
        assert!(validate_quote(&quote, 10_000_000, 50).is_ok());
    }

    #[test]
    fn route_binds_wallet_mints_input_minimum_and_zero_fees() {
        let sender = Pubkey::new_from_array([1; 32]);
        let source = Pubkey::new_from_array([2; 32]);
        let destination = Pubkey::new_from_array([3; 32]);
        let pool = Pubkey::new_from_array([4; 32]);
        let jupiter = address(JUPITER).unwrap();
        let keys = [
            sender,
            source,
            destination,
            address(USDT).unwrap(),
            spl_token::native_mint::id(),
            spl_token::id(),
            spl_token::id(),
            jupiter,
            Pubkey::find_program_address(&[b"__event_authority"], &jupiter).0,
            jupiter,
            address(RAYDIUM).unwrap(),
            spl_token::id(),
            pool,
            Pubkey::new_from_array([5; 32]),
            Pubkey::new_from_array([6; 32]),
            Pubkey::new_from_array([7; 32]),
            source,
            destination,
            sender,
        ];
        let mut ix = Instruction {
            program_id: jupiter,
            accounts: keys
                .into_iter()
                .map(|key| AccountMeta::new(key, key == sender))
                .collect(),
            data: STANDARD
                .decode("u2T6zDHErxSAlpgAAAAAABIrsgUAAAAAMgAAAAAAAQAAAGkQJwAB")
                .unwrap(),
        };
        let validate = |ix: &Instruction| {
            validate_jupiter_route(
                ix,
                sender,
                source,
                destination,
                pool,
                10_000_000,
                95_562_514,
                50,
            )
        };
        validate(&ix).unwrap();
        ix.data[26] = 1;
        assert!(validate(&ix).is_err());
        ix.data[26] = 0;
        ix.data[8] ^= 1;
        assert!(validate(&ix).is_err());
        ix.data[8] ^= 1;
        ix.accounts[2].pubkey = source;
        assert!(validate(&ix).is_err());
    }
}
