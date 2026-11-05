//! Issued secrets of one kind, presented as `<prefix>-<secret>`. Only the
//! digest of the secret, bound to its prefix, is kept; lookup goes by the
//! public prefix and then compares digests in constant time.
use std::collections::HashMap;

use super::error::OAuthError;
use crate::store::SecretText;
use crate::store::secret;

const PREFIX_SYMBOLS: usize = 8;
/// 32 symbols of five bits each: 160 random bits.
const SECRET_SYMBOLS: usize = 32;

pub(super) struct Ledger<R> {
    entries: HashMap<String, ([u8; 32], R)>,
}

impl<R> Default for Ledger<R> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }
}

impl<R> Ledger<R> {
    /// Stores `record` under a fresh secret and returns its public prefix and
    /// the only copy of the presentable text.
    pub(super) fn issue(&mut self, record: R) -> Result<(String, SecretText), OAuthError> {
        let prefix = loop {
            let candidate = secret::random_symbols(PREFIX_SYMBOLS)?;
            if !self.entries.contains_key(&candidate) {
                break candidate;
            }
        };
        let secret_part = secret::random_symbols(SECRET_SYMBOLS)?;
        let digest = secret::digest(&prefix, &secret_part);
        let text = SecretText::new(format!("{prefix}-{secret_part}"));
        self.entries.insert(prefix.clone(), (digest, record));
        Ok((prefix, text))
    }

    /// The record whose secret `text` presents, or `None` when it is
    /// malformed, unknown or carries the wrong secret for a known prefix.
    pub(super) fn find_mut(&mut self, text: &str) -> Option<&mut R> {
        let (prefix, secret_part) = text.split_once('-')?;
        let well_formed = secret::is_symbols(prefix, PREFIX_SYMBOLS)
            && secret::is_symbols(secret_part, SECRET_SYMBOLS);
        if !well_formed {
            return None;
        }
        let (digest, record) = self.entries.get_mut(prefix)?;
        secret::verifies(digest.as_slice(), prefix, secret_part).then_some(record)
    }

    pub(super) fn by_prefix_mut(&mut self, prefix: &str) -> Option<&mut R> {
        self.entries.get_mut(prefix).map(|(_, record)| record)
    }
}
