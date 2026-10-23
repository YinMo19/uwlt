use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::ffi::{CStr, CString, c_char, c_int};
use std::fmt;
use zeroize::Zeroizing;

unsafe extern "C" {
    fn uwlt_vault_available(error: *mut c_char, capacity: usize) -> c_int;
    fn uwlt_vault_save(
        id: *const c_char,
        label: *const c_char,
        address: *const c_char,
        solana_address: *const c_char,
        key: *const u8,
        reason: *const c_char,
        error: *mut c_char,
        capacity: usize,
    ) -> c_int;
    fn uwlt_vault_load(
        id: *const c_char,
        key: *mut u8,
        reason: *const c_char,
        error: *mut c_char,
        capacity: usize,
    ) -> c_int;
    fn uwlt_vault_delete(
        id: *const c_char,
        reason: *const c_char,
        error: *mut c_char,
        capacity: usize,
    ) -> c_int;
    fn uwlt_vault_sync(
        wallets: *const u8,
        wallets_length: usize,
        reason: *const c_char,
        derive_solana: unsafe extern "C" fn(*const u8, *const c_char, *mut c_char, usize) -> c_int,
        json: *mut *mut u8,
        json_length: *mut usize,
        error: *mut c_char,
        capacity: usize,
    ) -> c_int;
    fn uwlt_vault_free(json: *mut u8);
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncWallet {
    pub id: String,
    pub name: String,
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solana_address: Option<String>,
}

struct NativeJson {
    data: *mut u8,
    length: usize,
}

impl Drop for NativeJson {
    fn drop(&mut self) {
        // SAFETY: Native sync returns malloc-owned storage, or null on error.
        unsafe { uwlt_vault_free(self.data) }
    }
}

#[derive(Debug)]
pub struct VaultError {
    pub cancelled: bool,
    message: String,
}

impl fmt::Display for VaultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for VaultError {}

fn checked(call: impl FnOnce(*mut c_char, usize) -> c_int) -> Result<()> {
    let mut error = [0u8; 1024];
    let status = call(error.as_mut_ptr().cast(), error.len());
    if status == 0 {
        return Ok(());
    }
    let end = error
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(error.len());
    let message = if end == 0 {
        format!("Native Keychain operation failed (status {status})")
    } else {
        String::from_utf8_lossy(&error[..end]).into_owned()
    };
    Err(VaultError {
        cancelled: status == 2,
        message,
    }
    .into())
}

fn arguments(id: &str, reason: &str) -> Result<(CString, CString)> {
    Ok((
        CString::new(id).context("Wallet ID contains a NUL byte")?,
        CString::new(reason).context("Authorization reason contains a NUL byte")?,
    ))
}

/// Checks Touch ID capability only; never requests authorization or reads Keychain.
pub fn available() -> Result<()> {
    // SAFETY: Native code writes at most capacity bytes into this live buffer.
    checked(|error, capacity| unsafe { uwlt_vault_available(error, capacity) })
}

/// Adds a synchronizable item after one Touch ID authorization; never overwrites.
pub fn save(
    id: &str,
    name: &str,
    address: &str,
    solana_address: &str,
    key: &[u8; 32],
    reason: &str,
) -> Result<()> {
    let (id, reason) = arguments(id, reason)?;
    let label = CString::new(format!("uwlt: {name}"))?;
    let address = CString::new(address).context("Wallet address contains a NUL byte")?;
    let solana_address =
        CString::new(solana_address).context("Solana address contains a NUL byte")?;
    let mut encoded = Zeroizing::new([0u8; 64]);
    hex::encode_to_slice(key, encoded.as_mut())?;
    // SAFETY: Strings and the borrowed 64-byte hex key outlive the synchronous call.
    checked(|error, capacity| unsafe {
        uwlt_vault_save(
            id.as_ptr(),
            label.as_ptr(),
            address.as_ptr(),
            solana_address.as_ptr(),
            encoded.as_ptr(),
            reason.as_ptr(),
            error,
            capacity,
        )
    })
}

/// Explicitly authorizes Touch ID before reading a local or synchronized key.
pub fn load(id: &str, reason: &str) -> Result<Zeroizing<[u8; 32]>> {
    let (id, reason) = arguments(id, reason)?;
    let mut encoded = Zeroizing::new([0u8; 64]);
    // SAFETY: Native code copies exactly 64 hex bytes into this live output buffer.
    checked(|error, capacity| unsafe {
        uwlt_vault_load(
            id.as_ptr(),
            encoded.as_mut_ptr(),
            reason.as_ptr(),
            error,
            capacity,
        )
    })?;
    let mut key = Zeroizing::new([0u8; 32]);
    hex::decode_to_slice(encoded.as_slice(), key.as_mut())?;
    Ok(key)
}

/// Authorizes once, then deletes local and synchronized copies, including on other devices.
pub fn delete(id: &str, reason: &str) -> Result<()> {
    let (id, reason) = arguments(id, reason)?;
    // SAFETY: Native code consumes only live NUL-terminated strings and a bounded buffer.
    checked(|error, capacity| unsafe {
        uwlt_vault_delete(id.as_ptr(), reason.as_ptr(), error, capacity)
    })
}

// Called only during an already-authorized native sync. The key never enters
// metadata JSON; only the derived public address leaves this zeroizing buffer.
unsafe extern "C" fn derive_solana(
    encoded: *const u8,
    expected_tron: *const c_char,
    output: *mut c_char,
    capacity: usize,
) -> c_int {
    let derive = || -> Result<()> {
        ensure!(
            !encoded.is_null() && !expected_tron.is_null() && !output.is_null(),
            "missing address derivation buffer"
        );
        // SAFETY: native sync supplies exactly 64 hex bytes and a live C string.
        let encoded = unsafe { std::slice::from_raw_parts(encoded, 64) };
        let expected = unsafe { CStr::from_ptr(expected_tron) }.to_str()?;
        let mut key = Zeroizing::new([0u8; 32]);
        hex::decode_to_slice(encoded, key.as_mut())?;
        ensure!(
            crate::core::address(&key)? == expected,
            "wallet key/address mismatch"
        );
        let address = crate::core::solana_address(&key)?;
        ensure!(address.len() < capacity, "address buffer too small");
        // SAFETY: capacity includes the terminating NUL; source and output differ.
        unsafe {
            std::ptr::copy_nonoverlapping(address.as_ptr(), output.cast(), address.len());
            *output.add(address.len()) = 0;
        }
        Ok(())
    };
    if derive().is_ok() { 0 } else { 1 }
}

/// Migrates local keys and discovers metadata already downloaded by iCloud Keychain.
pub fn sync(wallets: &[SyncWallet]) -> Result<Vec<SyncWallet>> {
    let input = serde_json::to_vec(wallets)?;
    let reason = c"Synchronize uwlt wallets with iCloud Keychain";
    let mut json = NativeJson {
        data: std::ptr::null_mut(),
        length: 0,
    };
    // SAFETY: Input and output buffers remain live throughout the synchronous call.
    // The returned allocation belongs to json and is freed even if decoding fails.
    checked(|error, capacity| unsafe {
        uwlt_vault_sync(
            input.as_ptr(),
            input.len(),
            reason.as_ptr(),
            derive_solana,
            &mut json.data,
            &mut json.length,
            error,
            capacity,
        )
    })?;
    // SAFETY: Successful native sync returns a non-null allocation of json.length
    // bytes containing metadata only; json owns it until after deserialization.
    let bytes = unsafe { std::slice::from_raw_parts(json.data, json.length) };
    serde_json::from_slice(bytes).context("Cannot decode synchronized wallet metadata")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_requires_matching_key_and_a_bounded_public_address_buffer() {
        let key = [1u8; 32];
        let encoded = hex::encode(key);
        let address = CString::new(crate::core::address(&key).unwrap()).unwrap();
        let mut output = [0x7fu8; 45];
        let derive = |expected: &CStr, output: &mut [u8]| unsafe {
            derive_solana(
                encoded.as_ptr(),
                expected.as_ptr(),
                output.as_mut_ptr().cast(),
                output.len(),
            )
        };
        assert_ne!(derive(c"wrong-wallet", &mut output), 0);
        assert_eq!(output, [0x7f; 45]);
        assert_ne!(derive(&address, &mut output[..4]), 0);
        assert_eq!(output, [0x7f; 45]);
        assert_eq!(derive(&address, &mut output), 0);
        let actual = CStr::from_bytes_until_nul(&output)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(
            bs58::decode(actual).into_vec().unwrap(),
            hex::decode("8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c")
                .unwrap()
        );
    }
}
