//! Typed signature payloads. Field declaration order is part of the wire contract.
use crate::team::{InviteRecord, LinkRecord};
use crate::{NetworkError, Result, WireProfile};
use iroh::{EndpointId, SecretKey, Signature};
use serde::{Deserialize, Serialize};

mod sealed {
    pub trait Sealed {}
}

/// Only reviewed typed bodies may enter this signing domain.
pub trait RecordBody: sealed::Sealed + Serialize {
    const KIND: &'static str;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataRecord {
    pub name: String,
    pub relay: Option<String>,
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRecord {
    pub endpoint: EndpointId,
    pub person: String,
    pub machine: String,
    pub active: bool,
    pub ts: i64,
}

impl sealed::Sealed for InviteRecord {}
impl RecordBody for InviteRecord {
    const KIND: &'static str = "invite";
}
impl sealed::Sealed for LinkRecord {}
impl RecordBody for LinkRecord {
    const KIND: &'static str = "link";
}

impl sealed::Sealed for MetadataRecord {}
impl RecordBody for MetadataRecord {
    const KIND: &'static str = "meta";
}
impl sealed::Sealed for MemberRecord {}
impl RecordBody for MemberRecord {
    const KIND: &'static str = "member";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRecord<T> {
    pub body: T,
    pub by: EndpointId,
    pub sig: String,
}

/// Prefix includes its separator; body JSON is typed, never sorted-map JSON.
pub fn signing_payload<T: RecordBody>(
    profile: &WireProfile,
    team: &str,
    body: &T,
) -> Result<Vec<u8>> {
    profile.validate()?;
    // Keep the deployment prefix and team as separate signing-domain components.
    if !profile.signing_prefix.ends_with('/') {
        return Err(NetworkError::Invalid(
            "signing prefix requires a trailing separator",
        ));
    }
    if team.is_empty()
        || team.len() > 256
        || team.contains('/')
        || team.chars().any(char::is_control)
    {
        return Err(NetworkError::Invalid("invalid team signing domain"));
    }
    let mut payload = format!("{}{team}/{}/", profile.signing_prefix, T::KIND).into_bytes();
    serde_json::to_writer(&mut payload, body)?;
    Ok(payload)
}

impl<T: RecordBody> SignedRecord<T> {
    pub fn sign(body: T, key: &SecretKey, team: &str, profile: &WireProfile) -> Result<Self> {
        let signature = key.sign(&signing_payload(profile, team, &body)?);
        let mut sig = String::with_capacity(128);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in signature.to_bytes() {
            sig.push(HEX[(byte >> 4) as usize] as char);
            sig.push(HEX[(byte & 15) as usize] as char);
        }
        Ok(Self {
            body,
            by: key.public(),
            sig,
        })
    }

    /// Verifies the signer and domain, not permission to mutate a team's state.
    pub fn verify(&self, team: &str, profile: &WireProfile) -> Result<()> {
        if self.sig.len() != Signature::LENGTH * 2 {
            return Err(NetworkError::Invalid("signature length"));
        }
        let mut bytes = [0; Signature::LENGTH];
        for (slot, pair) in bytes.iter_mut().zip(self.sig.as_bytes().chunks_exact(2)) {
            let nibble = |byte| match byte {
                b'0'..=b'9' => Ok(byte - b'0'),
                b'a'..=b'f' => Ok(byte - b'a' + 10),
                _ => Err(NetworkError::Invalid("signature encoding")),
            };
            *slot = nibble(pair[0])? * 16 + nibble(pair[1])?;
        }
        self.by
            .verify(
                &signing_payload(profile, team, &self.body)?,
                &Signature::from_bytes(&bytes),
            )
            .map_err(|_| NetworkError::Invalid("signature verification failed"))
    }
}
