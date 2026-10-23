use std::collections::HashSet;

use super::route::{is_compute, validate_ata, validate_compute};
use super::*;
use solana_instruction::AccountMeta;

const JUPITER: &str = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4";
const LOOKUP_PROGRAM: &str = "AddressLookupTab1e1111111111111111111111111";

pub struct NativeSwapRoute {
    pub transaction: String,
    pub from: String,
    pub input_amount: u64,
    pub expected: u64,
    pub minimum: u64,
    pub fee_amount: u64,
    pub fee_recipient: String,
    pub expires_at: u64,
}

pub struct SwapQuote {
    pub summary: crate::swap::Summary,
    prepared: Prepared,
}

impl Client {
    /// Jupiter's official router and its supported CPI implementations, and the
    /// configured RPC, remain trust boundaries. No provider transaction is signed
    /// until its exact input/native recipient and on-chain NET minimum are bound.
    pub fn prepare_swap(&self, route: &NativeSwapRoute) -> Result<SwapQuote> {
        ensure!(now()? < route.expires_at, "swap quote expired");
        ensure!(
            route.input_amount > 0 && route.minimum > 0 && route.minimum <= route.expected,
            "invalid native swap amounts"
        );
        let sender = address(&route.from)?;
        let fee_recipient = address(&route.fee_recipient)?;
        ensure!(fee_recipient != sender, "swap fee recipient is the sender");
        self.validate_wallet(&sender)?;
        let mint = self.mint()?;
        let native_mint = spl_token::native_mint::id();
        self.validate_mint(&native_mint, 9)?;
        let source = spl_associated_token_account::get_associated_token_address(&sender, &mint);
        let destination =
            spl_associated_token_account::get_associated_token_address(&sender, &native_mint);
        let source_account = self.account(&source)?.context(
            "swap requires USDT in the sender's associated token account; transfers can aggregate other token accounts",
        )?;
        let source_balance = checked_token(&source_account, &mint, Some(&sender))?.amount;
        ensure!(
            source_balance >= route.input_amount,
            "insufficient USDT in the associated token account"
        );
        let existing_destination = self.account(&destination)?;
        let previous_wrapped_lamports =
            empty_wrapped_lamports(existing_destination.as_ref(), &sender)?;
        let mut transaction = decode_transaction(&route.transaction)?;
        validate_signer(&transaction.message, &sender)?;
        ensure!(
            transaction.signatures.len() == 1 && transaction.signatures[0] == Signature::default(),
            "refusing pre-signed swap transaction"
        );
        let mut instructions = self.resolve_swap_instructions(&transaction.message)?;
        let creates_destination =
            validate_instructions(&mut instructions, route, sender, source, destination)?;
        ensure!(
            existing_destination.is_some() || creates_destination,
            "swap has no wrapped SOL account setup"
        );
        // Do not expose any additional wallet-owned token account to router CPIs.
        // Existing nonzero wSOL is rejected above, not counted as swap proceeds.
        let mut checked = HashSet::from([source, destination, sender, fee_recipient]);
        let extra: Vec<String> = instructions
            .iter()
            .flat_map(|ix| &ix.accounts)
            .filter(|meta| meta.is_writable && checked.insert(meta.pubkey))
            .map(|meta| meta.pubkey.to_string())
            .collect();
        let token2022 = address("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb")?;
        for batch in extra.chunks(100) {
            let response = self.rpc(
                "getMultipleAccounts",
                json!([batch, {"encoding":"base64", "commitment":"confirmed"}]),
            )?;
            let values = response["value"]
                .as_array()
                .context("missing swap account states")?;
            ensure!(
                values.len() == batch.len(),
                "incomplete swap account states"
            );
            for value in values.iter().filter(|value| !value.is_null()) {
                let account = decode_account(value)?;
                if (account.owner == spl_token::id() || account.owner == token2022)
                    && account.data.len() >= TokenAccount::LEN
                {
                    let token = TokenAccount::unpack(&account.data[..TokenAccount::LEN])?;
                    ensure!(
                        token.owner != sender,
                        "swap exposes an unrelated wallet token account"
                    );
                }
            }
        }
        let rent = if creates_destination && existing_destination.is_none() {
            self.rent_lamports()?
        } else {
            0
        };
        let (hash, height) = self.latest_blockhash()?;
        // Keep the original account indexes and lookup selection; only replace the
        // checked Jupiter amount fields and refresh the blockhash before freezing.
        let compiled = match &mut transaction.message {
            VersionedMessage::Legacy(message) => {
                message.recent_blockhash = hash;
                &mut message.instructions
            }
            VersionedMessage::V0(message) => {
                message.recent_blockhash = hash;
                &mut message.instructions
            }
        };
        for (compiled, checked) in compiled.iter_mut().zip(instructions) {
            compiled.data = checked.data;
        }
        let fee = self.message_fee(&transaction.message)?;
        let before_sol = self.native_balance(&sender)?;
        let fee_budget = fee.checked_add(rent).context("swap fee budget overflow")?;
        ensure!(
            before_sol >= fee_budget,
            "insufficient SOL for swap fee and account rent"
        );
        let raw = bincode::serialize(&transaction)?;
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
        validate_system_account(&after_sender)?;
        let after_source = checked_token(&decode_account(&after[1])?, &mint, Some(&sender))?;
        ensure!(
            source_balance.checked_sub(after_source.amount) == Some(route.input_amount),
            "simulation does not spend the exact approved USDT input"
        );
        validate_unwrapped_account(&after[2])?;
        // The validated System transfer enforces the exact fee atomically. Do not
        // compare the shared provider fee wallet across independently sampled
        // slots: payments from other users can change it before simulation.
        ensure!(
            u128::from(after_sender.lamports) + u128::from(fee)
                >= u128::from(before_sol)
                    + u128::from(route.minimum)
                    + u128::from(previous_wrapped_lamports),
            "simulated native SOL output is below the approved NET minimum"
        );
        self.fresh(&hash, Some(height), route.expires_at)?;
        Ok(SwapQuote {
            summary: crate::swap::Summary {
                venue: "MetaMask / Jupiter",
                input: amount::format(route.input_amount, 6),
                expected: amount::format(route.expected, 9),
                minimum: amount::format(route.minimum, 9),
                fee_budget: amount::format(fee_budget, 9),
                approvals: 0,
                token: "USDT",
                native: "SOL",
            },
            prepared: Prepared {
                message: transaction.message,
                sender,
                last_valid_block_height: Some(height),
                expires_at: route.expires_at,
                from: sender.to_string(),
                to: sender.to_string(),
                amount: amount::format(route.input_amount, 6),
                symbol: "USDT".to_owned(),
                fee: amount::format(fee, 9),
                rent: amount::format(rent, 9),
            },
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

    fn resolve_swap_instructions(&self, message: &VersionedMessage) -> Result<Vec<Instruction>> {
        let mut writable = Vec::new();
        let mut readonly = Vec::new();
        if let VersionedMessage::V0(message) = message {
            let mut seen = HashSet::new();
            for lookup in &message.address_table_lookups {
                ensure!(
                    seen.insert(lookup.account_key),
                    "duplicate address lookup table"
                );
                let result = self.rpc(
                    "getAccountInfo",
                    json!([lookup.account_key.to_string(),
                    {"encoding":"base64", "commitment":"confirmed"}]),
                )?;
                let slot = result["context"]["slot"]
                    .as_u64()
                    .context("lookup context slot missing")?;
                let account = decode_account(&result["value"])?;
                let addresses = lookup_addresses(&account, slot)?;
                for (indexes, output) in [
                    (&lookup.writable_indexes, &mut writable),
                    (&lookup.readonly_indexes, &mut readonly),
                ] {
                    for index in indexes {
                        output.push(
                            *addresses
                                .get(usize::from(*index))
                                .context("lookup account index unavailable")?,
                        );
                    }
                }
            }
        }
        resolve_message(message, &writable, &readonly)
    }
}

// Solana AddressLookupTable's bincode ProgramState::LookupTable metadata is 56
// bytes. Existing entries are immutable; extensions cannot change frozen indexes.
fn lookup_addresses(account: &Account, slot: u64) -> Result<Vec<Pubkey>> {
    let data = &account.data;
    ensure!(
        account.owner == address(LOOKUP_PROGRAM)? && !account.executable,
        "invalid address lookup table owner"
    );
    ensure!(
        data.len() >= 56 && (data.len() - 56) % 32 == 0 && (data.len() - 56) / 32 <= 256,
        "invalid address lookup table size"
    );
    ensure!(
        data[..4] == 1u32.to_le_bytes() && data[21] <= 1,
        "invalid lookup table metadata"
    );
    ensure!(
        u64::from_le_bytes(data[4..12].try_into()?) == u64::MAX,
        "deactivated address lookup table"
    );
    let extended_slot = u64::from_le_bytes(data[12..20].try_into()?);
    ensure!(
        extended_slot <= slot,
        "lookup table has a future extension slot"
    );
    let count = (data.len() - 56) / 32;
    ensure!(
        usize::from(data[20]) <= count,
        "invalid lookup extension index"
    );
    let active = if extended_slot == slot {
        usize::from(data[20])
    } else {
        count
    };
    data[56..56 + active * 32]
        .chunks_exact(32)
        .map(|bytes| Ok(Pubkey::new_from_array(bytes.try_into()?)))
        .collect()
}

fn resolve_message(
    message: &VersionedMessage,
    writable: &[Pubkey],
    readonly: &[Pubkey],
) -> Result<Vec<Instruction>> {
    let header = message.header();
    let static_keys = message.static_account_keys();
    let signers = usize::from(header.num_required_signatures);
    let ro_signed = usize::from(header.num_readonly_signed_accounts);
    let ro_unsigned = usize::from(header.num_readonly_unsigned_accounts);
    ensure!(
        signers <= static_keys.len()
            && ro_signed <= signers
            && ro_unsigned <= static_keys.len() - signers,
        "invalid swap message header"
    );
    let keys: Vec<Pubkey> = static_keys
        .iter()
        .chain(writable)
        .chain(readonly)
        .copied()
        .collect();
    ensure!(
        keys.len() <= 256 && keys.iter().collect::<HashSet<_>>().len() == keys.len(),
        "duplicate or excessive swap account keys"
    );
    message
        .instructions()
        .iter()
        .map(|ix| {
            let program_index = usize::from(ix.program_id_index);
            ensure!(
                program_index >= signers,
                "signer cannot be an instruction program"
            );
            let program_id = *keys
                .get(program_index)
                .context("program index out of range")?;
            let accounts = ix
                .accounts
                .iter()
                .map(|index| {
                    let index = usize::from(*index);
                    let is_signer = index < signers;
                    let is_writable = if is_signer {
                        index < signers - ro_signed
                    } else if index < static_keys.len() {
                        index < static_keys.len() - ro_unsigned
                    } else {
                        index < static_keys.len() + writable.len()
                    };
                    Ok(AccountMeta {
                        pubkey: *keys.get(index).context("account index out of range")?,
                        is_signer,
                        is_writable,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Instruction {
                program_id,
                accounts,
                data: ix.data.clone(),
            })
        })
        .collect()
}

fn validate_instructions(
    instructions: &mut [Instruction],
    route: &NativeSwapRoute,
    sender: Pubkey,
    source: Pubkey,
    destination: Pubkey,
) -> Result<bool> {
    let jupiter = address(JUPITER)?;
    let fee_recipient = address(&route.fee_recipient)?;
    let mut budgets = HashSet::new();
    let (mut created, mut swapped, mut closed, mut paid) = (false, false, false, false);
    for ix in instructions {
        ensure!(
            ix.accounts
                .iter()
                .all(|a| !a.is_signer || a.pubkey == sender),
            "swap requests another signer"
        );
        if is_compute(ix) {
            ensure!(!created && !swapped, "reordered compute budget instruction");
            validate_compute(ix, &mut budgets)?;
        } else if ix.program_id == spl_associated_token_account::id() {
            ensure!(!created && !swapped, "unexpected extra token account setup");
            let (ata, owner, mint) = validate_ata(ix, &sender)?;
            ensure!(
                ata == destination && owner == sender && mint == spl_token::native_mint::id(),
                "swap creates an unapproved token account"
            );
            ensure!(
                ix.accounts[0].is_writable && ix.accounts[1].is_writable,
                "readonly ATA setup"
            );
            created = true;
        } else if ix.program_id == jupiter {
            ensure!(
                !swapped && !closed,
                "multiple or reordered swap instructions"
            );
            strengthen_jupiter_route(ix, route, sender, source, destination)?;
            swapped = true;
        } else if ix.program_id == spl_token::id() {
            ensure!(
                swapped && !closed && ix.data == [9] && ix.accounts.len() == 3,
                "unsupported swap token instruction"
            );
            let a = &ix.accounts;
            ensure!(
                a[0].pubkey == destination
                    && a[0].is_writable
                    && a[1].pubkey == sender
                    && a[1].is_writable
                    && a[2].pubkey == sender
                    && a[2].is_signer,
                "swap must unwrap SOL to the sender"
            );
            closed = true;
        } else if ix.program_id == system_program::id() {
            ensure!(
                closed && !paid && route.fee_amount > 0,
                "unexpected native transfer"
            );
            ensure!(
                ix.data.len() == 12
                    && ix.data[..4] == 2u32.to_le_bytes()
                    && u64::from_le_bytes(ix.data[4..12].try_into()?) == route.fee_amount
                    && ix.accounts.len() == 2,
                "invalid MetaMask fee transfer"
            );
            let a = &ix.accounts;
            ensure!(
                a[0].pubkey == sender
                    && a[0].is_signer
                    && a[0].is_writable
                    && a[1].pubkey == fee_recipient
                    && a[1].is_writable
                    && !a[1].is_signer,
                "unapproved MetaMask fee recipient"
            );
            paid = true;
        } else {
            bail!(
                "unsupported swap program {}; refusing blind signing",
                ix.program_id
            );
        }
    }
    ensure!(
        swapped && closed && (paid || route.fee_amount == 0),
        "swap must include one route, native unwrapping and the approved fee"
    );
    Ok(created)
}

fn discriminator(name: &str) -> [u8; 8] {
    solana_sha256_hasher::hash(name.as_bytes()).to_bytes()[..8]
        .try_into()
        .unwrap()
}

// Account/argument layouts: https://github.com/jup-ag/jupiter-cpi/blob/main/idl.json
// V2: https://github.com/sevenlabs-hq/carbon/blob/main/decoders/jupiter-swap-decoder/src/instructions/route_v2.rs
fn strengthen_jupiter_route(
    ix: &mut Instruction,
    route: &NativeSwapRoute,
    sender: Pubkey,
    source: Pubkey,
    destination: Pubkey,
) -> Result<()> {
    let jupiter = address(JUPITER)?;
    ensure!(
        ix.program_id == jupiter && ix.data.len() >= 8,
        "invalid Jupiter instruction"
    );
    let a = &ix.accounts;
    let event = Pubkey::find_program_address(&[b"__event_authority"], &jupiter).0;
    let native = spl_token::native_mint::id();
    let token = spl_token::id();
    let mint = address(USDT)?;
    let (input_offset, output_offset, slippage_offset, fee_offset, fee_len) = if ix.data[..8]
        == discriminator("global:shared_accounts_route")
    {
        ensure!(
            a.len() >= 13 && ix.data.len() >= 13,
            "truncated shared Jupiter route"
        );
        let authority = Pubkey::find_program_address(&[b"authority", &[ix.data[8]]], &jupiter).0;
        ensure!(
            a[0].pubkey == token
                && a[1].pubkey == authority
                && a[2].pubkey == sender
                && a[2].is_signer
                && a[3].pubkey == source
                && a[3].is_writable
                && a[6].pubkey == destination
                && a[6].is_writable
                && a[7].pubkey == mint
                && a[8].pubkey == native
                && a[9].pubkey == jupiter
                && (a[10].pubkey == jupiter
                    || a[10].pubkey == address("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb")?)
                && a[11].pubkey == event
                && a[12].pubkey == jupiter,
            "shared Jupiter route changes mints, recipient, authority or fees"
        );
        let end = route_plan_end(&ix.data, 9, false)?;
        ensure!(
            ix.data.len() == end + 19,
            "trailing or truncated Jupiter arguments"
        );
        (end, end + 8, end + 16, end + 18, 1)
    } else if ix.data[..8] == discriminator("global:route") {
        ensure!(a.len() >= 9, "truncated Jupiter route accounts");
        ensure!(
            a[0].pubkey == token
                && a[1].pubkey == sender
                && a[1].is_signer
                && a[2].pubkey == source
                && a[2].is_writable
                && a[3].pubkey == destination
                && a[3].is_writable
                && (a[4].pubkey == jupiter || a[4].pubkey == destination)
                && a[5].pubkey == native
                && a[6].pubkey == jupiter
                && a[7].pubkey == event
                && a[8].pubkey == jupiter,
            "Jupiter route changes recipient, authority or fees"
        );
        let end = route_plan_end(&ix.data, 8, false)?;
        ensure!(
            ix.data.len() == end + 19,
            "trailing or truncated Jupiter arguments"
        );
        (end, end + 8, end + 16, end + 18, 1)
    } else if ix.data[..8] == discriminator("global:route_v2") {
        ensure!(
            a.len() >= 10 && ix.data.len() >= 34,
            "truncated Jupiter V2 route"
        );
        ensure!(
            a[0].pubkey == sender
                && a[0].is_signer
                && a[1].pubkey == source
                && a[1].is_writable
                && a[2].pubkey == destination
                && a[2].is_writable
                && a[3].pubkey == mint
                && a[4].pubkey == native
                && a[5].pubkey == token
                && a[6].pubkey == token
                && (a[7].pubkey == jupiter || a[7].pubkey == destination)
                && a[8].pubkey == event
                && a[9].pubkey == jupiter,
            "Jupiter V2 route changes mints, recipient, authority or fees"
        );
        ensure!(
            route_plan_end(&ix.data, 30, true)? == ix.data.len(),
            "trailing Jupiter V2 arguments"
        );
        (8, 16, 24, 26, 4)
    } else {
        bail!(
            "unsupported Jupiter route layout; exact-output and token-ledger routes are not authorized"
        );
    };
    let input = u64::from_le_bytes(ix.data[input_offset..input_offset + 8].try_into()?);
    let output = u64::from_le_bytes(ix.data[output_offset..output_offset + 8].try_into()?);
    let slippage = u16::from_le_bytes(ix.data[slippage_offset..slippage_offset + 2].try_into()?);
    let gross_expected = route
        .expected
        .checked_add(route.fee_amount)
        .context("gross swap output overflow")?;
    let gross_minimum = route
        .minimum
        .checked_add(route.fee_amount)
        .context("gross swap minimum overflow")?;
    ensure!(
        input == route.input_amount
            && output == gross_expected
            && slippage < 10_000
            && ix.data[fee_offset..fee_offset + fee_len]
                .iter()
                .all(|v| *v == 0),
        "Jupiter changes exact input, quoted output, slippage or router fees"
    );
    let encoded_minimum =
        u64::try_from(u128::from(output) * u128::from(10_000 - slippage) / 10_000)?;
    // Never weaken an already stronger router minimum. The fixed native fee is
    // paid after unwrap, so the on-chain gross floor must cover NET minimum + fee.
    let floor = gross_minimum.max(encoded_minimum);
    ensure!(
        floor > 0 && floor <= gross_expected,
        "invalid encoded Jupiter minimum"
    );
    ix.data[output_offset..output_offset + 8].copy_from_slice(&floor.to_le_bytes());
    ix.data[slippage_offset..slippage_offset + 2].copy_from_slice(&0u16.to_le_bytes());
    Ok(())
}

// Parse the entire Borsh vector, never locate amount fields by counting backwards:
// trailing bytes could otherwise hide a weaker minimum consumed by the router.
// Layouts through variant 202 are pinned to Jupiter's program-owned Anchor IDL
// C88XWfp26heEmDkmfSzeXP7Fd7GQJ2j9dDTUsyiZbUTa at mainnet slot 460396130.
// Public repository IDLs lag the deployed program. Unknown/dynamic variants
// still fail closed; only fully decoded fields may precede the amount arguments.
fn route_plan_end(data: &[u8], offset: usize, v2: bool) -> Result<usize> {
    let mut cursor = Cursor {
        data,
        position: offset,
    };
    let count = u32::from_le_bytes(cursor.take(4)?.try_into()?);
    ensure!(count > 0 && count <= 64, "invalid Jupiter route step count");
    for _ in 0..count {
        let tag = cursor.byte()?;
        match tag {
            0..=7
            | 9..=11
            | 13..=14
            | 19..=20
            | 22
            | 25..=26
            | 30..=32
            | 34..=38
            | 40
            | 46
            | 48..=57
            | 59
            | 62..=63
            | 65..=70
            | 72..=74
            | 76..=80
            | 83..=84
            | 88
            | 90..=93
            | 96..=102
            | 105
            | 108..=109
            | 112..=115
            | 124
            | 128
            | 130..=131
            | 133..=134
            | 137..=140
            | 142..=144
            | 147..=150
            | 154
            | 156
            | 158
            | 163
            | 169
            | 173
            | 175..=176
            | 179..=180
            | 188
            | 195
            | 198
            | 200..=202 => {}
            8
            | 12
            | 15..=18
            | 21
            | 23..=24
            | 27..=28
            | 39
            | 58
            | 60..=61
            | 64
            | 85
            | 89
            | 94..=95
            | 104
            | 106..=107
            | 110
            | 116..=117
            | 119
            | 121
            | 125
            | 127
            | 129
            | 136
            | 141
            | 145
            | 151..=153
            | 160
            | 162
            | 164..=166
            | 168
            | 174
            | 177..=178
            | 181..=182
            | 184
            | 189
            | 192..=194
            | 196..=197
            | 199 => {
                cursor.boolean()?;
            }
            29 | 122 => {
                cursor.take(16)?;
            }
            33 | 41 => {
                cursor.take(4)?;
            }
            42 => {
                cursor.byte()?;
                cursor.boolean()?;
                cursor.boolean()?;
            }
            43 | 135 => {
                cursor.take(10)?;
            }
            44..=45 => {
                cursor.take(5)?;
            }
            71 => {
                cursor.take(2)?;
            }
            81..=82 | 159 => {
                cursor.take(8)?;
            }
            86 => {
                cursor.boolean()?;
                cursor.byte()?;
            }
            87 | 118 | 157 => {
                cursor.take(8)?;
                cursor.boolean()?;
            }
            123 => {
                cursor.take(48)?;
            }
            126 => {
                cursor.boolean()?;
                cursor.take(16)?;
            }
            161 => {
                cursor.boolean()?;
                cursor.take(4)?;
            }
            155 | 167 | 187 | 191 => {
                cursor.boolean()?;
                cursor.boolean()?;
            }
            170 => {
                cursor.byte()?;
            }
            171 => {
                ensure!(cursor.byte()? < 3, "invalid Sanctum swap type");
            }
            132 | 172 => {
                ensure!(cursor.byte()? < 8, "invalid Hylo swap type");
            }
            183 => {
                cursor.boolean()?;
                match cursor.byte()? {
                    0 => {}
                    1 => {
                        cursor.byte()?;
                    }
                    _ => bail!("invalid optional external liquidity source"),
                }
            }
            185 | 186 | 190 => {
                let bytes = match tag {
                    185 => 56,
                    186 => 64,
                    _ => 80,
                };
                cursor.take(bytes)?;
                cursor.boolean()?;
            }
            _ => bail!("unsupported Jupiter swap variant {tag}"),
        }
        let weight = if v2 {
            u16::from_le_bytes(cursor.take(2)?.try_into()?)
        } else {
            u16::from(cursor.byte()?)
        };
        ensure!(
            weight > 0 && weight <= if v2 { 10_000 } else { 100 },
            "invalid Jupiter route weight"
        );
        let input = cursor.byte()?;
        let output = cursor.byte()?;
        ensure!(input != output, "cyclic Jupiter route step");
    }
    Ok(cursor.position)
}

struct Cursor<'a> {
    data: &'a [u8],
    position: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .context("Jupiter route offset overflow")?;
        let bytes = self
            .data
            .get(self.position..end)
            .context("truncated Jupiter route plan")?;
        self.position = end;
        Ok(bytes)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn boolean(&mut self) -> Result<()> {
        ensure!(self.byte()? <= 1, "invalid Jupiter route boolean or side");
        Ok(())
    }
}

fn validate_system_account(account: &Account) -> Result<()> {
    ensure!(
        account.owner == system_program::id() && account.data.is_empty() && !account.executable,
        "swap changes wallet ownership"
    );
    Ok(())
}

fn empty_wrapped_lamports(account: Option<&Account>, sender: &Pubkey) -> Result<u64> {
    if let Some(account) = account {
        ensure!(
            checked_token(account, &spl_token::native_mint::id(), Some(sender))?.amount == 0,
            "empty the existing wrapped SOL account before swapping to native SOL"
        );
        Ok(account.lamports)
    } else {
        Ok(0)
    }
}

fn validate_unwrapped_account(value: &Value) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    // Simulation may return the zero-lamport system account before RPC purging.
    let account = decode_account(value)?;
    validate_system_account(&account)?;
    ensure!(
        account.lamports == 0,
        "simulation leaves wrapped SOL instead of native SOL"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> NativeSwapRoute {
        NativeSwapRoute {
            transaction: String::new(),
            from: String::new(),
            input_amount: 1_000_000,
            expected: 9_461_836,
            minimum: 9_414_526,
            fee_amount: 83_521,
            fee_recipient: "4cLUBQKZgCv2AqGXbh8ncGhrDRcicUe3WSDzjgPY2oTA".into(),
            expires_at: u64::MAX,
        }
    }

    fn shared_route() -> (Instruction, Pubkey, Pubkey, Pubkey) {
        let sender = address("Bw2qw7uMTpDt3mksUyZnkcCMWPCv1wcTCMS7NbAiSKrD").unwrap();
        let source = spl_associated_token_account::get_associated_token_address(
            &sender,
            &address(USDT).unwrap(),
        );
        let destination = spl_associated_token_account::get_associated_token_address(
            &sender,
            &spl_token::native_mint::id(),
        );
        let jupiter = address(JUPITER).unwrap();
        // Actual MetaMask Jupiter shared_accounts_route: Deriverse -> Kipseli.
        let data = STANDARD
            .decode("wSCbM0HWnIEEAgAAAKEBAQAAAGQAAbUAZAECQEIPAAAAAACNppEAAAAAADIAAA==")
            .unwrap();
        let keys = [
            spl_token::id(),
            Pubkey::find_program_address(&[b"authority", &[4]], &jupiter).0,
            sender,
            source,
            Pubkey::new_from_array([4; 32]),
            Pubkey::new_from_array([5; 32]),
            destination,
            address(USDT).unwrap(),
            spl_token::native_mint::id(),
            jupiter,
            jupiter,
            Pubkey::find_program_address(&[b"__event_authority"], &jupiter).0,
            jupiter,
        ];
        (
            Instruction {
                program_id: jupiter,
                data,
                accounts: keys
                    .into_iter()
                    .map(|key| AccountMeta::new(key, key == sender))
                    .collect(),
            },
            sender,
            source,
            destination,
        )
    }

    #[test]
    fn observed_multihop_binds_exact_input_and_net_minimum_on_chain() {
        let (mut ix, sender, source, destination) = shared_route();
        let end = route_plan_end(&ix.data, 9, false).unwrap();
        let approved = route();
        strengthen_jupiter_route(&mut ix, &approved, sender, source, destination).unwrap();
        assert_eq!(
            u64::from_le_bytes(ix.data[end + 8..end + 16].try_into().unwrap()),
            approved.minimum + approved.fee_amount
        );
        assert_eq!(&ix.data[end + 16..end + 19], &[0, 0, 0]);
        for index in [2, 3, 6, 7, 8, 9, 11, 12] {
            let (mut bad, _, _, _) = shared_route();
            bad.accounts[index].pubkey = Pubkey::new_from_array([42; 32]);
            assert!(
                strengthen_jupiter_route(&mut bad, &approved, sender, source, destination).is_err()
            );
        }
        for index in [end, end + 8, end + 18] {
            let (mut bad, _, _, _) = shared_route();
            bad.data[index] ^= 1;
            assert!(
                strengthen_jupiter_route(&mut bad, &approved, sender, source, destination).is_err()
            );
        }
        let (mut bad, _, _, _) = shared_route();
        bad.data.extend([0; 19]);
        assert!(
            strengthen_jupiter_route(&mut bad, &approved, sender, source, destination).is_err()
        );
    }

    #[test]
    fn deployed_byreal_and_lend_routes_preserve_net_minimum() {
        let (mut ix, sender, source, destination) = shared_route();
        let old_end = route_plan_end(&ix.data, 9, false).unwrap();
        // ByrealDynamicV3 -> JupiterLendDexSwap -> TesseraVV2.
        let plan = [
            3, 0, 0, 0, 156, 100, 0, 1, 165, 1, 100, 1, 2, 178, 0, 100, 2, 3,
        ];
        ix.data.splice(9..old_end, plan);
        let amount_offset = 9 + plan.len();
        let approved = route();
        strengthen_jupiter_route(&mut ix, &approved, sender, source, destination).unwrap();
        assert_eq!(
            u64::from_le_bytes(
                ix.data[amount_offset + 8..amount_offset + 16]
                    .try_into()
                    .unwrap()
            ),
            approved.minimum + approved.fee_amount
        );
        assert_eq!(&ix.data[amount_offset + 16..], &[0, 0, 0]);
        ix.data[18] = 2;
        assert!(strengthen_jupiter_route(&mut ix, &approved, sender, source, destination).is_err());
    }

    #[test]
    fn extended_route_payloads_reject_truncation_and_invalid_enums() {
        for (tag, payload) in [
            (183, vec![1, 0]),
            (183, vec![0, 1, 7]),
            (185, vec![0; 57]),
            (186, vec![0; 65]),
            (190, vec![0; 81]),
        ] {
            let mut plan = vec![1, 0, 0, 0, tag];
            plan.extend(payload);
            plan.extend([100, 0, 1]);
            assert_eq!(route_plan_end(&plan, 0, false).unwrap(), plan.len());
            plan.pop();
            assert!(route_plan_end(&plan, 0, false).is_err());
        }
        for plan in [
            vec![1, 0, 0, 0, 171, 3, 100, 0, 1],
            vec![1, 0, 0, 0, 172, 8, 100, 0, 1],
            vec![1, 0, 0, 0, 183, 0, 2, 100, 0, 1],
            vec![1, 0, 0, 0, 155, 2, 0, 100, 0, 1],
            vec![1, 0, 0, 0, 203, 100, 0, 1],
        ] {
            assert!(route_plan_end(&plan, 0, false).is_err());
        }
    }

    fn instructions() -> (Vec<Instruction>, Pubkey, Pubkey, Pubkey) {
        let (ix, sender, source, destination) = shared_route();
        let close = spl_token::instruction::close_account(
            &spl_token::id(),
            &destination,
            &sender,
            &sender,
            &[],
        )
        .unwrap();
        let fee = system_instruction::transfer(
            &sender,
            &address(&route().fee_recipient).unwrap(),
            route().fee_amount,
        );
        (vec![ix, close, fee], sender, source, destination)
    }

    #[test]
    fn native_instruction_sequence_rejects_wrong_fee_unwrap_signer_and_program() {
        let (mut good, sender, source, destination) = instructions();
        validate_instructions(&mut good, &route(), sender, source, destination).unwrap();
        for mutation in 0..7 {
            let (mut bad, _, _, _) = instructions();
            match mutation {
                0 => bad[1].accounts[1].pubkey = source,
                1 => bad[2].accounts[1].pubkey = source,
                2 => bad[2].data[4] ^= 1,
                3 => bad[0].accounts[4].is_signer = true,
                4 => bad[1].program_id = Pubkey::new_from_array([42; 32]),
                5 => {
                    bad.remove(1);
                }
                _ => bad.swap(1, 2),
            }
            assert!(
                validate_instructions(&mut bad, &route(), sender, source, destination).is_err(),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn unwrap_accepts_absent_or_closed_account_only() {
        assert!(validate_unwrapped_account(&Value::Null).is_ok());
        let closed = json!({"lamports":0,"owner":system_program::id().to_string(),"data":["","base64"],"executable":false});
        assert!(validate_unwrapped_account(&closed).is_ok());
        for (field, value) in [
            ("lamports", json!(1)),
            ("owner", json!(spl_token::id().to_string())),
            (
                "data",
                json!([STANDARD.encode([0; TokenAccount::LEN]), "base64"]),
            ),
            ("executable", json!(true)),
        ] {
            let mut bad = closed.clone();
            bad[field] = value;
            assert!(validate_unwrapped_account(&bad).is_err());
        }
    }

    #[test]
    fn lookup_tables_enforce_owner_activation_and_immutable_index_bounds() {
        let mut table = Account {
            owner: address(LOOKUP_PROGRAM).unwrap(),
            lamports: 1,
            executable: false,
            data: vec![0; 120],
        };
        table.data[..4].copy_from_slice(&1u32.to_le_bytes());
        table.data[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
        table.data[12..20].copy_from_slice(&10u64.to_le_bytes());
        table.data[20] = 1;
        table.data[56..88].fill(1);
        table.data[88..120].fill(2);
        assert_eq!(lookup_addresses(&table, 10).unwrap().len(), 1);
        assert_eq!(lookup_addresses(&table, 11).unwrap().len(), 2);
        assert!(lookup_addresses(&table, 9).is_err());
        table.data[20] = 3;
        assert!(lookup_addresses(&table, 11).is_err());
        table.data[20] = 1;
        table.data[4] = 0;
        assert!(lookup_addresses(&table, 11).is_err());
        table.data[4] = 255;
        table.owner = system_program::id();
        assert!(lookup_addresses(&table, 11).is_err());
    }

    #[test]
    fn v0_resolution_preserves_lookup_permissions_and_rejects_aliases() {
        let sender = Pubkey::new_from_array([1; 32]);
        let loaded = Pubkey::new_from_array([2; 32]);
        let program = address(JUPITER).unwrap();
        let message = VersionedMessage::V0(solana_message::v0::Message {
            header: solana_message::MessageHeader {
                num_required_signatures: 1,
                num_readonly_signed_accounts: 0,
                num_readonly_unsigned_accounts: 1,
            },
            account_keys: vec![sender, program],
            recent_blockhash: Hash::default(),
            instructions: vec![solana_message::compiled_instruction::CompiledInstruction {
                program_id_index: 1,
                accounts: vec![0, 2],
                data: vec![],
            }],
            address_table_lookups: vec![],
        });
        let resolved = resolve_message(&message, &[loaded], &[]).unwrap();
        assert!(resolved[0].accounts[0].is_signer);
        assert!(resolved[0].accounts[1].is_writable);
        assert!(!resolved[0].accounts[1].is_signer);
        assert!(!resolve_message(&message, &[], &[loaded]).unwrap()[0].accounts[1].is_writable);
        assert!(resolve_message(&message, &[sender], &[]).is_err());
        assert!(resolve_message(&message, &[], &[]).is_err());
    }

    #[test]
    fn v2_multihop_minimum_and_router_fees_are_bound() {
        let (shared, sender, source, destination) = shared_route();
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
            shared.accounts[11].pubkey,
            jupiter,
        ];
        let approved = route();
        let mut data = discriminator("global:route_v2").to_vec();
        data.extend(approved.input_amount.to_le_bytes());
        data.extend((approved.expected + approved.fee_amount).to_le_bytes());
        data.extend(50u16.to_le_bytes());
        data.extend([0; 4]);
        data.extend(2u32.to_le_bytes());
        data.extend([161, 1, 1, 0, 0, 0, 16, 39, 0, 1, 181, 0, 16, 39, 1, 2]);
        let mut ix = Instruction {
            program_id: jupiter,
            data,
            accounts: keys
                .into_iter()
                .map(|key| AccountMeta::new(key, key == sender))
                .collect(),
        };
        let original = ix.clone();
        strengthen_jupiter_route(&mut ix, &approved, sender, source, destination).unwrap();
        assert_eq!(
            u64::from_le_bytes(ix.data[16..24].try_into().unwrap()),
            approved.minimum + approved.fee_amount
        );
        for position in [26, 28] {
            let mut bad = original.clone();
            bad.data[position] = 1;
            assert!(
                strengthen_jupiter_route(&mut bad, &approved, sender, source, destination).is_err()
            );
        }
        let mut bad = original;
        bad.data[34] = 255;
        assert!(
            strengthen_jupiter_route(&mut bad, &approved, sender, source, destination).is_err()
        );
    }

    #[test]
    fn nonzero_existing_wrapped_sol_is_never_destroyed() {
        let sender = Pubkey::new_from_array([1; 32]);
        let mut token = TokenAccount {
            mint: spl_token::native_mint::id(),
            owner: sender,
            amount: 1,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::Some(100),
            delegated_amount: 0,
            close_authority: COption::None,
        };
        let mut account = Account {
            owner: spl_token::id(),
            lamports: 101,
            executable: false,
            data: vec![0; TokenAccount::LEN],
        };
        TokenAccount::pack(token, &mut account.data).unwrap();
        assert!(empty_wrapped_lamports(Some(&account), &sender).is_err());
        token.amount = 0;
        TokenAccount::pack(token, &mut account.data).unwrap();
        assert_eq!(
            empty_wrapped_lamports(Some(&account), &sender).unwrap(),
            101
        );
    }
}
