use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

#[derive(Serialize, Deserialize)]
pub struct Wallet {
    pub id: String,
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solana_address: Option<String>,
}

impl Wallet {
    pub fn ethereum_address(&self) -> Result<String> {
        let bytes: [u8; 21] = bs58::decode(&self.address)
            .with_check(Some(0x41))
            .into_vec()?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid stored wallet address"))?;
        Ok(alloy::primitives::Address::from_slice(&bytes[1..]).to_checksum(None))
    }

    pub fn solana_address(&self) -> Result<String> {
        let address = self.solana_address.as_ref().context(
            "wallet has no Solana address; run `uwlt sync --network solana` once with Touch ID to migrate it",
        )?;
        let mut bytes = [0u8; 32];
        ensure!(
            bs58::decode(address).onto(&mut bytes[..])? == bytes.len(),
            "invalid stored Solana address"
        );
        Ok(address.clone())
    }
}

pub struct Store {
    path: PathBuf,
    wallets: BTreeMap<String, Wallet>,
    _lock: File,
}

impl Store {
    pub fn open() -> Result<Self> {
        let directory = match std::env::var_os("UWLT_HOME") {
            Some(path) => PathBuf::from(path),
            None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?)
                .join("Library/Application Support/uwlt"),
        };
        fs::create_dir_all(&directory).context("create wallet directory")?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(directory.join("wallets.lock"))?;
        lock.lock().context("lock wallet metadata")?;
        let path = directory.join("wallets.json");
        let wallets = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("read wallet metadata")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error).context("read wallet metadata"),
        };
        Ok(Self {
            path,
            wallets,
            _lock: lock,
        })
    }

    pub fn find(&self, name: &str) -> Option<&Wallet> {
        self.wallets.get(name)
    }

    pub fn get(&self, name: &str) -> Result<&Wallet> {
        self.wallets
            .get(name)
            .with_context(|| format!("wallet {name:?} not found; use `uwlt create {name}`"))
    }

    pub fn entries(&self) -> impl Iterator<Item = (&String, &Wallet)> {
        self.wallets.iter()
    }

    pub fn check_new(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        ensure!(
            !self.wallets.contains_key(name),
            "wallet {name:?} already exists"
        );
        Ok(())
    }

    pub fn insert(&mut self, name: String, wallet: Wallet) -> Result<()> {
        self.wallets.insert(name, wallet);
        self.persist(&self.wallets)
    }

    pub fn remove(&mut self, name: &str) -> Result<()> {
        self.wallets.remove(name);
        self.persist(&self.wallets)
    }

    pub fn replace(&mut self, wallets: Vec<crate::vault::SyncWallet>) -> Result<()> {
        let mut replacement = BTreeMap::new();
        for wallet in wallets {
            ensure!(
                !replacement.contains_key(&wallet.name),
                "duplicate wallet name {:?} in iCloud Keychain; local metadata was not changed. Resolve duplicate names on the original device: reimport under a distinct name and remove the original, then run `uwlt sync` again",
                wallet.name
            );
            replacement.insert(
                wallet.name,
                Wallet {
                    id: wallet.id,
                    address: wallet.address,
                    solana_address: wallet.solana_address,
                },
            );
        }
        self.persist(&replacement)?;
        self.wallets = replacement;
        Ok(())
    }

    fn persist(&self, wallets: &BTreeMap<String, Wallet>) -> Result<()> {
        let directory = self.path.parent().context("wallet directory missing")?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer_pretty(&mut file, wallets)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(&self.path).context("save wallet metadata")?;
        File::open(directory)?.sync_all()?;
        Ok(())
    }
}

pub fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_lowercase()),
        "wallet name must contain only lowercase letters a-z"
    );
    Ok(())
}

pub fn read_key(path: Option<&Path>) -> Result<Zeroizing<[u8; 32]>> {
    let text = Zeroizing::new(match path {
        Some(path) => fs::read_to_string(path).context("read private key file")?,
        None => rpassword::prompt_password("Private key: ")?,
    });
    let value = text.trim();
    let value = value.strip_prefix("0x").unwrap_or(value);
    if value.len() != 64 {
        bail!("private key must be 64 hexadecimal characters");
    }
    let mut key = Zeroizing::new([0; 32]);
    hex::decode_to_slice(value, key.as_mut()).context("private key must be hexadecimal")?;
    Ok(key)
}

pub fn write_backup(path: &Path, key: &[u8; 32]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create backup {} (must not already exist)", path.display()))?;
    let encoded = Zeroizing::new(hex::encode(key));
    file.write_all(encoded.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn backup_is_restorable_owner_only_and_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wallet.key");
        let key = [0x27; 32];
        write_backup(&path, &key).unwrap();
        assert_eq!(*read_key(Some(&path)).unwrap(), key);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(write_backup(&path, &[0x42; 32]).is_err());
        assert_eq!(*read_key(Some(&path)).unwrap(), key);
    }
}
