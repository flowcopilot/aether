//! The byte forms aether signs, and their one signature check.
//!
//! Each signed message starts with a domain such as `aether/claim/v1`, then
//! its fields in a fixed order. A string or a list carries its length as a
//! big-endian `u64`; a time is its Unix seconds as a big-endian `u64`; a key
//! or a nonce is its raw bytes.

use std::time::SystemTime;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

/// The bytes of one message to sign, built field by field.
pub struct SignedBytes(Vec<u8>);

impl SignedBytes {
    #[must_use]
    pub fn new(domain: &[u8]) -> Self {
        Self(domain.to_vec())
    }

    #[must_use]
    pub fn string(self, value: &str) -> Self {
        self.bytes(value.as_bytes())
    }

    /// A list of strings: its count, then each string.
    #[must_use]
    pub fn strings<'a>(self, values: impl ExactSizeIterator<Item = &'a str>) -> Self {
        let count = self.length(values.len());
        values.fold(count, Self::string)
    }

    /// Bytes of any length, after their length.
    #[must_use]
    pub fn bytes(self, value: &[u8]) -> Self {
        self.length(value.len()).raw(value)
    }

    /// Bytes of a fixed width, such as a key or a nonce.
    #[must_use]
    pub fn raw(mut self, value: &[u8]) -> Self {
        self.0.extend_from_slice(value);
        self
    }

    #[must_use]
    pub fn byte(mut self, value: u8) -> Self {
        self.0.push(value);
        self
    }

    /// A time as its Unix seconds; a time before 1970 signs as 0.
    #[must_use]
    pub fn time(self, value: SystemTime) -> Self {
        self.u64(aether_core::time::unix_seconds(value).unwrap_or(0))
    }

    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.0
    }

    /// A `u64` holds every `usize` on the platforms Rust supports; the
    /// saturation only keeps the conversion total.
    fn length(self, length: usize) -> Self {
        self.u64(u64::try_from(length).unwrap_or(u64::MAX))
    }

    fn u64(self, value: u64) -> Self {
        self.raw(&value.to_be_bytes())
    }
}

/// The signature of `message` with `signing_key`.
#[must_use]
pub fn sign(signing_key: &SigningKey, message: &[u8]) -> Vec<u8> {
    signing_key.sign(message).to_bytes().to_vec()
}

/// The key is not a key, the signature is not a signature, or the key did
/// not sign the message.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Forged;

/// Checks that `signing_key` signed `message` with `signature`.
///
/// # Errors
///
/// [`Forged`] when it did not.
pub fn verify(
    signing_key: &[u8; 32],
    message: &[u8],
    signature: &[u8],
) -> Result<(), Forged> {
    let key = VerifyingKey::from_bytes(signing_key).map_err(|_| Forged)?;
    let signature = Signature::try_from(signature).map_err(|_| Forged)?;
    key.verify_strict(message, &signature).map_err(|_| Forged)
}
