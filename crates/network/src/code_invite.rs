//! Explicit dictionary and context configuration for private invitation codes.
use crate::{NetworkError, Result, WireProfile};
use std::collections::HashSet;

pub struct Codebook {
    words: Vec<String>,
}
impl Codebook {
    /// The caller supplies the expected SHA-256 of the exact dictionary bytes.
    pub fn from_bytes(bytes: &[u8], expected: [u8; 32]) -> Result<Self> {
        if bytes.is_empty()
            || bytes.len() > 262_144
            || ring::digest::digest(&ring::digest::SHA256, bytes).as_ref() != expected
        {
            return Err(NetworkError::Invalid(
                "invalid code dictionary bytes or digest",
            ));
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|_| NetworkError::Invalid("invalid code dictionary encoding"))?;
        let mut words = Vec::new();
        let mut seen = HashSet::new();
        for word in text.lines() {
            if words.len() == 4096
                || word.is_empty()
                || word.len() > 1023
                || !word.bytes().all(|byte| byte.is_ascii_lowercase())
                || !seen.insert(word)
            {
                return Err(NetworkError::Invalid("invalid code dictionary entry"));
            }
            words.push(word.to_owned());
        }
        if words.is_empty() {
            return Err(NetworkError::Invalid("empty code dictionary"));
        }
        Ok(Self { words })
    }
}

pub struct CodePolicy {
    book: Codebook,
    endpoint_context: String,
    secret_context: String,
}

/// Secret-bearing values deliberately have no Debug or generic serialization.
pub struct CanonicalCode(String);
impl CanonicalCode {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

pub struct DerivedCode {
    endpoint: iroh::SecretKey,
    bearer: String,
}

/// Opaque proof that a selected code matches the authenticated JOIN endpoint.
/// The invitation hash is privileged and deliberately has no Debug/Serialize.
pub struct CodeAnchor {
    invitation_hash: String,
    endpoint_context: String,
    secret_context: String,
}
impl CodeAnchor {
    pub(crate) fn invitation_hash(&self) -> &str {
        &self.invitation_hash
    }
    pub(crate) fn matches_profile(&self, profile: &WireProfile) -> bool {
        profile.validate().is_ok()
            && self.endpoint_context == profile.invitation_endpoint_context
            && self.secret_context == profile.invitation_secret_context
    }
}
impl DerivedCode {
    pub fn endpoint_key(&self) -> &iroh::SecretKey {
        &self.endpoint
    }
    pub fn bearer_secret(&self) -> &str {
        &self.bearer
    }
    pub fn invitation_key(&self) -> [u8; 32] {
        *blake3::hash(self.bearer.as_bytes()).as_bytes()
    }
}

impl CodePolicy {
    /// Pass the remote ID from the actual JOIN connection, not response JSON.
    pub fn authenticate_endpoint(
        &self,
        code: &CanonicalCode,
        authenticated: iroh::EndpointId,
    ) -> Result<CodeAnchor> {
        let derived = self.derive(code)?;
        if derived.endpoint_key().public() != authenticated {
            return Err(NetworkError::Invalid("invitation endpoint mismatch"));
        }
        Ok(CodeAnchor {
            invitation_hash: data_encoding::HEXLOWER.encode(&derived.invitation_key()),
            endpoint_context: self.endpoint_context.clone(),
            secret_context: self.secret_context.clone(),
        })
    }
    /// Stable binding to the actual ordered dictionary and derivation contexts.
    /// Length-prefixing keeps word and context boundaries unambiguous.
    pub fn binding(&self) -> [u8; 32] {
        let mut hash = blake3::Hasher::new_derive_key("foundry code policy binding v1");
        hash.update(&(self.book.words.len() as u64).to_be_bytes());
        for value in self
            .book
            .words
            .iter()
            .chain([&self.endpoint_context, &self.secret_context])
        {
            hash.update(&(value.len() as u64).to_be_bytes());
            hash.update(value.as_bytes());
        }
        *hash.finalize().as_bytes()
    }

    /// Checks this policy's derivation contexts, not transport or signing policy.
    /// The store binds the remaining wire-profile fields separately.
    pub fn matches_profile(&self, profile: &WireProfile) -> bool {
        profile.validate().is_ok()
            && self.endpoint_context == profile.invitation_endpoint_context
            && self.secret_context == profile.invitation_secret_context
    }

    pub fn new(book: Codebook, profile: &WireProfile) -> Result<Self> {
        profile.validate()?;
        Ok(Self {
            book,
            endpoint_context: profile.invitation_endpoint_context.clone(),
            secret_context: profile.invitation_secret_context.clone(),
        })
    }

    pub fn normalize(&self, input: &str) -> Result<CanonicalCode> {
        if input.len() > 4096 {
            return Err(NetworkError::Invalid("invitation code exceeds input limit"));
        }
        let mut parts = Vec::with_capacity(4);
        for part in input
            .split(|c: char| !c.is_ascii_alphabetic())
            .filter(|s| !s.is_empty())
        {
            if parts.len() == 4 {
                return Err(NetworkError::Invalid("invitation code requires four words"));
            }
            let word = part.to_ascii_lowercase();
            if !self.book.words.contains(&word) {
                return Err(NetworkError::Invalid(
                    "invitation code contains unknown word",
                ));
            }
            parts.push(word);
        }
        if parts.len() != 4 {
            return Err(NetworkError::Invalid("invitation code requires four words"));
        }
        Ok(CanonicalCode(parts.join("-")))
    }

    pub fn generate(&self) -> Result<CanonicalCode> {
        self.generate_with(|bytes| {
            getrandom::fill(bytes)
                .map_err(|_| NetworkError::Invalid("invitation entropy unavailable"))
        })
    }

    fn generate_with(
        &self,
        mut entropy: impl FnMut(&mut [u8; 4]) -> Result<()>,
    ) -> Result<CanonicalCode> {
        let count = self.book.words.len() as u32;
        let cutoff = u32::MAX - u32::MAX % count;
        let mut parts = Vec::with_capacity(4);
        for _ in 0..4 {
            loop {
                let mut bytes = [0; 4];
                entropy(&mut bytes)?;
                let sample = u32::from_le_bytes(bytes);
                if sample < cutoff {
                    parts.push(self.book.words[(sample % count) as usize].as_str());
                    break;
                }
            }
        }
        Ok(CanonicalCode(parts.join("-")))
    }

    pub fn derive(&self, code: &CanonicalCode) -> Result<DerivedCode> {
        // A code from another policy must still belong to this configured dictionary.
        self.normalize(code.expose())?;
        let seed = blake3::derive_key(&self.endpoint_context, code.0.as_bytes());
        let secret = blake3::derive_key(&self.secret_context, code.0.as_bytes());
        let bearer = data_encoding::HEXLOWER.encode(&secret[..16]);
        Ok(DerivedCode {
            endpoint: iroh::SecretKey::from_bytes(&seed),
            bearer,
        })
    }
}

#[cfg(test)]
#[path = "code_invite_tests.rs"]
mod tests;
