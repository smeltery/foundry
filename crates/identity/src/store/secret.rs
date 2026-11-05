//! Credential bytes. Plaintext exists only in the [`SecretText`] handed to the
//! caller at issue and in the text a holder presents; the database keeps a
//! SHA-256 digest bound to the public prefix. Secrets are uniformly random, so
//! a fast digest suffices; no password-derived value is ever stored.
use std::fmt;

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::error::StoreError;

/// Upper-case letters and digits without `I`, `O`, `0` and `1`. Its 32 symbols
/// let each random byte, masked to five bits, pick one without bias.
const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

const DIGEST_DOMAIN: &[u8] = b"foundry.identity.credential.v1";

/// Plaintext credential text, returned once at issue. It has no `Display`, and
/// its `Debug` output is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretText(String);

impl SecretText {
    pub(crate) fn new(text: String) -> Self {
        Self(text)
    }

    /// The text to hand to the holder. Never log or persist it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretText(<redacted>)")
    }
}

/// `count` random symbols from the operating system's secure source.
pub(crate) fn random_symbols(count: usize) -> Result<String, StoreError> {
    let mut bytes = vec![0u8; count];
    getrandom::fill(&mut bytes)
        .map_err(|error| StoreError::unavailable(format!("random source: {error}")))?;
    Ok(bytes
        .iter()
        .map(|byte| char::from(ALPHABET[usize::from(byte & 31)]))
        .collect())
}

/// Whether `text` is exactly `count` alphabet symbols.
pub(crate) fn is_symbols(text: &str, count: usize) -> bool {
    text.len() == count && text.bytes().all(|byte| ALPHABET.contains(&byte))
}

/// The stored digest. Binding the prefix means a digest copied onto another
/// row never verifies there.
pub(crate) fn digest(prefix: &str, secret: &str) -> [u8; 32] {
    Sha256::new()
        .chain_update(DIGEST_DOMAIN)
        .chain_update([0])
        .chain_update(prefix)
        .chain_update([0])
        .chain_update(secret)
        .finalize()
        .into()
}

/// Compares in constant time over the digest, so timing never reveals how much
/// of a guess matched.
pub(crate) fn verifies(stored: &[u8], prefix: &str, secret: &str) -> bool {
    stored.ct_eq(&digest(prefix, secret)).into()
}
