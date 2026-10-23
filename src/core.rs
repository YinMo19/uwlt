//! Trust Wallet Core 4.8.4 C ABI. All cryptography stays in the pinned static library.
use std::{
    ffi::{CStr, c_char, c_void},
    ptr::NonNull,
    slice,
};

use anyhow::{Context, Result, ensure};
use prost::Message;
use serde_json::Value;
use zeroize::{Zeroize, Zeroizing};

const TRON: u32 = 195;
const SOLANA: u32 = 501;
const ED25519: u32 = 1;

unsafe extern "C" {
    fn TWPrivateKeyCreate() -> *mut c_void;
    fn TWPrivateKeyCreateWithData(data: *mut c_void) -> *mut c_void;
    fn TWPrivateKeyIsValid(data: *mut c_void, curve: u32) -> bool;
    fn TWPrivateKeyDelete(key: *mut c_void);
    fn TWPrivateKeyData(key: *mut c_void) -> *mut c_void;
    fn TWPrivateKeySign(key: *mut c_void, message: *mut c_void, curve: u32) -> *mut c_void;
    fn TWCoinTypeDeriveAddress(coin: u32, key: *mut c_void) -> *mut c_void;
    fn TWAnySignerSign(input: *mut c_void, coin: u32) -> *mut c_void;
    fn TWDataCreateWithBytes(bytes: *const u8, size: usize) -> *mut c_void;
    fn TWDataSize(data: *mut c_void) -> usize;
    fn TWDataBytes(data: *mut c_void) -> *mut u8;
    fn TWDataDelete(data: *mut c_void);
    fn TWStringUTF8Bytes(string: *mut c_void) -> *const c_char;
    fn TWStringDelete(string: *mut c_void);
}

struct PrivateKey(NonNull<c_void>);

impl PrivateKey {
    fn from_bytes(key: &[u8; 32]) -> Result<Self> {
        let data = Data::from_bytes(key)?;
        ensure!(
            unsafe { TWPrivateKeyIsValid(data.0.as_ptr(), 0) },
            "invalid secp256k1 private key"
        );
        // SAFETY: data owns a live TWData containing exactly 32 key bytes.
        let pointer = unsafe { TWPrivateKeyCreateWithData(data.0.as_ptr()) };
        Ok(Self(NonNull::new(pointer).context("invalid private key")?))
    }
}

impl Drop for PrivateKey {
    fn drop(&mut self) {
        // Wallet Core PrivateKey::~PrivateKey invokes cleanup()/memzero.
        unsafe { TWPrivateKeyDelete(self.0.as_ptr()) };
    }
}

struct Data(NonNull<c_void>);

impl Data {
    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        // SAFETY: Wallet Core copies this borrowed buffer before returning.
        Self::from_raw(unsafe { TWDataCreateWithBytes(bytes.as_ptr(), bytes.len()) })
    }

    fn from_raw(pointer: *mut c_void) -> Result<Self> {
        Ok(Self(
            NonNull::new(pointer).context("Wallet Core returned null data")?,
        ))
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: pointers remain owned by self and no mutation occurs while borrowed.
        unsafe {
            let len = TWDataSize(self.0.as_ptr());
            if len == 0 {
                return &[];
            }
            slice::from_raw_parts(TWDataBytes(self.0.as_ptr()), len)
        }
    }
}

impl Drop for Data {
    fn drop(&mut self) {
        // Pinned TWDataDelete uses memzero before freeing its entire byte vector.
        unsafe { TWDataDelete(self.0.as_ptr()) };
    }
}

struct NativeString(NonNull<c_void>);

impl NativeString {
    fn from_raw(pointer: *mut c_void) -> Result<Self> {
        Ok(Self(
            NonNull::new(pointer).context("Wallet Core returned null string")?,
        ))
    }

    fn as_str(&self) -> Result<&str> {
        // SAFETY: returned NUL-terminated bytes are borrowed from this live TWString.
        let pointer = unsafe { TWStringUTF8Bytes(self.0.as_ptr()) };
        ensure!(!pointer.is_null(), "Wallet Core returned null UTF-8 bytes");
        unsafe { CStr::from_ptr(pointer) }
            .to_str()
            .context("Wallet Core returned invalid UTF-8")
    }
}

impl Drop for NativeString {
    fn drop(&mut self) {
        unsafe { TWStringDelete(self.0.as_ptr()) };
    }
}

pub fn generate() -> Result<Zeroizing<[u8; 32]>> {
    // Wallet Core uses its platform CSPRNG and fails closed if entropy fails.
    let pointer = unsafe { TWPrivateKeyCreate() };
    let key = PrivateKey(NonNull::new(pointer).context("Wallet Core key generation failed")?);
    let data = Data::from_raw(unsafe { TWPrivateKeyData(key.0.as_ptr()) })?;
    ensure!(
        data.bytes().len() == 32,
        "Wallet Core returned an invalid key size"
    );
    let mut output = Zeroizing::new([0; 32]);
    output.copy_from_slice(data.bytes());
    Ok(output)
}

pub fn address(key: &[u8; 32]) -> Result<String> {
    let key = PrivateKey::from_bytes(key)?;
    let address = NativeString::from_raw(unsafe { TWCoinTypeDeriveAddress(TRON, key.0.as_ptr()) })?;
    Ok(address.as_str()?.to_owned())
}

pub fn solana_address(key: &[u8; 32]) -> Result<String> {
    let key = PrivateKey::from_bytes(key)?;
    let address =
        NativeString::from_raw(unsafe { TWCoinTypeDeriveAddress(SOLANA, key.0.as_ptr()) })?;
    Ok(address.as_str()?.to_owned())
}

/// Ed25519 signs the serialized Solana message itself, not a prehashed digest.
pub fn solana_sign(key: &[u8; 32], message: &[u8]) -> Result<[u8; 64]> {
    let key = PrivateKey::from_bytes(key)?;
    let message = Data::from_bytes(message)?;
    let signature =
        Data::from_raw(unsafe { TWPrivateKeySign(key.0.as_ptr(), message.0.as_ptr(), ED25519) })?;
    signature
        .bytes()
        .try_into()
        .context("Wallet Core returned an invalid Ed25519 signature")
}

// Exact field numbers from wallet-core/4.8.4/src/proto/Tron.proto.
// Tron does NOT support TWAnySignerSignJSON in 4.8.4. The protobuf C API invokes
// the same native Signer::signRawJson with txID/raw_data_hex/raw_data validation.
#[derive(Message)]
struct SigningInput {
    #[prost(bytes = "vec", tag = "2")]
    private_key: Vec<u8>,
    #[prost(string, tag = "4")]
    raw_json: String,
}

impl Drop for SigningInput {
    fn drop(&mut self) {
        self.private_key.zeroize();
    }
}

#[derive(Message)]
struct SigningOutput {
    #[prost(string, tag = "5")]
    json: String,
    #[prost(int32, tag = "6")]
    error: i32,
    #[prost(string, tag = "7")]
    error_message: String,
}

pub fn sign(key: &[u8; 32], unsigned: &Value) -> Result<Value> {
    // Require node bytes too; Wallet Core checks both bytes and JSON against txID.
    ensure!(
        unsigned
            .get("raw_data_hex")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty()),
        "unsigned transaction is missing raw_data_hex"
    );
    let input = SigningInput {
        private_key: key.to_vec(),
        raw_json: unsigned.to_string(),
    };
    // Preallocate once so secret-containing serialization never reallocates.
    let mut encoded = Zeroizing::new(Vec::with_capacity(input.encoded_len()));
    input
        .encode(&mut *encoded)
        .context("encode Wallet Core signing input")?;
    let input_data = Data::from_bytes(&encoded)?;
    let output_data = Data::from_raw(unsafe { TWAnySignerSign(input_data.0.as_ptr(), TRON) })?;
    let output =
        SigningOutput::decode(output_data.bytes()).context("decode Wallet Core signing output")?;
    ensure!(
        output.error == 0,
        "Wallet Core signing failed (code {}): {}",
        output.error,
        output.error_message
    );
    serde_json::from_str(&output.json).context("parse signed transaction JSON")
}

#[cfg(test)]
mod tests;
