use super::*;
use bincode::Options;
use solana_sdk::instruction::AccountMeta;
use std::collections::HashSet;

/// Exact approved SPL-token transfer legs; destinations are token accounts.
/// Quotes do not authorize arbitrary instructions.
#[derive(Clone, Debug)]
pub struct RouteTransfer {
    pub destination: String,
    pub amount: u64,
}
pub struct RemoteRoute {
    pub transaction: String,
    pub from: String,
    pub input_mint: String,
    pub input_amount: u64,
    pub recipient: String,
    pub expires_at: u64,
    pub last_valid_block_height: Option<u64>,
    pub transfers: Vec<RouteTransfer>,
}

impl Client {
    pub fn prepare_route(&self, route: &RemoteRoute) -> Result<Prepared> {
        ensure!(
            self.network == Network::Mainnet,
            "bridge routes require Solana mainnet"
        );
        let sender = address(&route.from)?;
        self.validate_wallet(&sender)?;
        let mint = self.mint()?;
        ensure!(
            address(&route.input_mint)? == mint && route.input_amount > 0,
            "route input mint or amount is invalid"
        );
        ensure!(now()? < route.expires_at, "route quote expired");
        let transaction = decode_transaction(&route.transaction)?;
        validate_signer(&transaction.message, &sender)?;
        ensure!(
            transaction.signatures.len() == 1 && transaction.signatures[0] == Signature::default(),
            "refusing pre-signed remote transaction"
        );
        self.fresh(
            self.network,
            transaction.message.recent_blockhash(),
            route.last_valid_block_height,
            route.expires_at,
        )?;
        let instructions = Self::resolve_instructions(&transaction.message)?;
        let mut remaining = route.transfers.clone();
        let mut rent = 0u64;
        let mut created = HashSet::new();
        let mut spent = 0u64;
        let mut budgets = HashSet::new();
        for instruction in &instructions {
            if is_compute(instruction) {
                validate_compute(instruction, &mut budgets)?;
            } else if instruction.program_id == spl_associated_token_account::id() {
                let (ata, owner, account_mint) = validate_ata(instruction, &sender)?;
                ensure!(
                    account_mint == mint
                        && route
                            .transfers
                            .iter()
                            .any(|leg| leg.destination == ata.to_string()),
                    "route creates an unapproved token account"
                );
                ensure!(created.insert(ata), "duplicate token account creation");
                if let Some(account) = self.account(&ata)? {
                    checked_token(&account, &mint, Some(&owner))?;
                } else {
                    rent = rent
                        .checked_add(self.rent_lamports()?)
                        .context("rent overflow")?;
                }
            } else if instruction.program_id == spl_token::id() {
                let (source, destination, authority, amount, checked_mint) =
                    parse_transfer(instruction)?;
                ensure!(authority == sender, "unexpected SPL transfer authority");
                if let Some(checked_mint) = checked_mint {
                    ensure!(checked_mint == mint, "wrong transfer mint");
                }
                let source_account = self
                    .account(&source)?
                    .context("route source token account missing")?;
                let source_token = checked_token(&source_account, &mint, Some(&sender))?;
                ensure!(
                    source_token.amount >= amount,
                    "insufficient source token account balance"
                );
                if !created.contains(&destination) {
                    checked_token(
                        &self
                            .account(&destination)?
                            .context("route destination token account missing")?,
                        &mint,
                        None,
                    )?;
                }
                consume_leg(&mut remaining, destination, amount)?;
                spent = spent.checked_add(amount).context("route input overflow")?;
            } else {
                bail!(
                    "unsupported remote route program {}; refusing blind signing",
                    instruction.program_id
                );
            }
        }
        ensure!(
            spent == route.input_amount && remaining.is_empty(),
            "route does not exactly match the approved transfer legs"
        );
        self.finish(
            transaction.message,
            sender,
            route.recipient.clone(),
            spent,
            6,
            "USDT",
            0,
            rent,
            route.last_valid_block_height,
            route.expires_at,
        )
    }

    fn resolve_instructions(message: &VersionedMessage) -> Result<Vec<Instruction>> {
        if let VersionedMessage::V0(message) = message {
            ensure!(
                message.address_table_lookups.is_empty(),
                "Solana deposit route with address lookup tables is unsupported"
            );
        }
        let keys = message.static_account_keys();
        let static_len = keys.len();
        ensure!(
            keys.len() <= 256 && keys.iter().collect::<HashSet<_>>().len() == keys.len(),
            "duplicate or excessive transaction account keys"
        );
        let header = message.header();
        let signer_count = usize::from(header.num_required_signatures);
        let readonly_signed = usize::from(header.num_readonly_signed_accounts);
        let readonly_unsigned = usize::from(header.num_readonly_unsigned_accounts);
        ensure!(
            signer_count <= static_len
                && readonly_signed <= signer_count
                && readonly_unsigned <= static_len - signer_count,
            "invalid transaction header"
        );
        message
            .instructions()
            .iter()
            .map(|ix| {
                let program = *keys
                    .get(usize::from(ix.program_id_index))
                    .context("program account index out of range")?;
                ensure!(
                    usize::from(ix.program_id_index) >= signer_count,
                    "signer cannot be an instruction program"
                );
                let accounts = ix
                    .accounts
                    .iter()
                    .map(|index| {
                        let index = usize::from(*index);
                        let key = *keys
                            .get(index)
                            .context("instruction account index out of range")?;
                        let signer = index < signer_count;
                        let writable = if signer {
                            index < signer_count - readonly_signed
                        } else {
                            index < static_len - readonly_unsigned
                        };
                        Ok(AccountMeta {
                            pubkey: key,
                            is_signer: signer,
                            is_writable: writable,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(Instruction {
                    program_id: program,
                    accounts,
                    data: ix.data.clone(),
                })
            })
            .collect()
    }
}

pub(crate) fn decode_transaction(encoded: &str) -> Result<VersionedTransaction> {
    ensure!(
        encoded.len() <= 1644,
        "remote transaction exceeds Solana packet size"
    );
    let bytes = STANDARD
        .decode(encoded)
        .context("invalid transaction base64")?;
    ensure!(
        bytes.len() <= 1232,
        "remote transaction exceeds Solana packet size"
    );
    let transaction: VersionedTransaction = bincode::options()
        .with_fixint_encoding()
        .with_limit(1232)
        .reject_trailing_bytes()
        .deserialize(&bytes)
        .context("invalid serialized Solana transaction")?;
    ensure!(
        bincode::serialize(&transaction)? == bytes,
        "noncanonical remote transaction encoding"
    );
    Ok(transaction)
}
fn consume_leg(remaining: &mut Vec<RouteTransfer>, destination: Pubkey, amount: u64) -> Result<()> {
    ensure!(amount > 0, "zero-amount route transfer");
    let position = remaining
        .iter()
        .position(|leg| leg.destination == destination.to_string() && leg.amount == amount)
        .context("unapproved route transfer destination or amount")?;
    remaining.remove(position);
    Ok(())
}
pub(super) fn is_compute(ix: &Instruction) -> bool {
    ix.program_id == solana_sdk::compute_budget::id()
}
pub(super) fn validate_compute(ix: &Instruction, seen: &mut HashSet<u8>) -> Result<()> {
    ensure!(
        ix.accounts.is_empty(),
        "compute budget instruction has accounts"
    );
    let tag = *ix.data.first().context("empty compute instruction")?;
    ensure!(seen.insert(tag), "duplicate compute budget instruction");
    match tag {
        2 => {
            ensure!(ix.data.len() == 5, "invalid compute limit");
            let units = u32::from_le_bytes(ix.data[1..5].try_into()?);
            ensure!(
                units > 0 && units <= 1_400_000,
                "invalid compute unit limit"
            );
        }
        3 => {
            ensure!(ix.data.len() == 9, "invalid compute price");
        }
        _ => bail!("unsupported compute budget instruction"),
    }
    Ok(())
}
pub(super) fn validate_ata(ix: &Instruction, sender: &Pubkey) -> Result<(Pubkey, Pubkey, Pubkey)> {
    ensure!(
        (ix.data.is_empty() || ix.data == [1]) && ix.accounts.len() == 6,
        "unsupported associated token account instruction"
    );
    let a = &ix.accounts;
    ensure!(
        a[0].pubkey == *sender
            && a[0].is_signer
            && a[4].pubkey == system_program::id()
            && a[5].pubkey == spl_token::id(),
        "invalid associated token account payer or programs"
    );
    ensure!(
        a[1].pubkey
            == spl_associated_token_account::get_associated_token_address(
                &a[2].pubkey,
                &a[3].pubkey
            ),
        "wrong associated token address"
    );
    Ok((a[1].pubkey, a[2].pubkey, a[3].pubkey))
}
fn parse_transfer(ix: &Instruction) -> Result<(Pubkey, Pubkey, Pubkey, u64, Option<Pubkey>)> {
    match ix.data.first() {
        Some(3) => {
            ensure!(
                ix.data.len() == 9 && ix.accounts.len() == 3 && ix.accounts[2].is_signer,
                "invalid SPL transfer"
            );
            Ok((
                ix.accounts[0].pubkey,
                ix.accounts[1].pubkey,
                ix.accounts[2].pubkey,
                u64::from_le_bytes(ix.data[1..9].try_into()?),
                None,
            ))
        }
        Some(12) => {
            ensure!(
                ix.data.len() == 10
                    && ix.data[9] == 6
                    && ix.accounts.len() == 4
                    && ix.accounts[3].is_signer,
                "invalid checked USDT transfer"
            );
            Ok((
                ix.accounts[0].pubkey,
                ix.accounts[2].pubkey,
                ix.accounts[3].pubkey,
                u64::from_le_bytes(ix.data[1..9].try_into()?),
                Some(ix.accounts[1].pubkey),
            ))
        }
        _ => bail!(
            "unsupported SPL instruction; approvals, authority changes and account closures are forbidden"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transfer_authorization_is_exact_and_single_use() {
        let key = Pubkey::new_from_array([9; 32]);
        let mut legs = vec![RouteTransfer {
            destination: key.to_string(),
            amount: 100,
        }];
        assert!(consume_leg(&mut legs, key, 101).is_err());
        consume_leg(&mut legs, key, 100).unwrap();
        assert!(consume_leg(&mut legs, key, 100).is_err());
    }
    #[test]
    fn remote_wire_rejects_trailing_bytes_and_extra_signers() {
        let sender = Pubkey::new_from_array([1; 32]);
        let message = VersionedMessage::Legacy(Message::new_with_blockhash(
            &[],
            Some(&sender),
            &Hash::default(),
        ));
        let tx = VersionedTransaction {
            signatures: vec![Signature::default()],
            message,
        };
        let mut bytes = bincode::serialize(&tx).unwrap();
        assert!(decode_transaction(&STANDARD.encode(&bytes)).is_ok());
        bytes.push(0);
        assert!(decode_transaction(&STANDARD.encode(bytes)).is_err());
        let mut message = tx.message;
        if let VersionedMessage::Legacy(message) = &mut message {
            message.header.num_required_signatures = 2;
        }
        assert!(validate_signer(&message, &sender).is_err());
    }
}
