//! Bounded identity commitments only; no native approval or restored execution authority.
use super::capture::CaptureError;
use foundry_contracts::{
    MAX_PROMPT_BYTES,
    context::{ContextDigest, ContextId},
};
use serde::{Deserialize, Serialize};

pub const MAX_ORIGIN_BYTES: usize = 16_384;
pub const MAX_WITNESS_BYTES: u64 = 262_144;
pub const MAX_LOGICAL_RECORDS: u64 = 1024;
pub const MAX_PREFIX_RECORDS: u64 = 4096;
pub const MAX_MAPPING_REFS: u64 = 4096;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreshOpeningIdentity {
    pub session_id: ContextId,
    pub delivery_attempt_id: ContextId,
    pub prompt_digest: ContextDigest,
    pub prompt_bytes: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoredBaselineIdentity {
    pub session_id: ContextId,
    pub checkpoint_id: ContextId,
    pub generation: u64,
    pub source_attempt_id: ContextId,
    pub original_prompt_attempt_id: ContextId,
    pub prompt_digest: ContextDigest,
    pub prompt_bytes: u64,
    pub record_digest: ContextDigest,
    pub payload_digest: ContextDigest,
    pub witness_digest: ContextDigest,
    pub witness_bytes: u64,
    pub history_binding: ContextDigest,
    pub prefix_binding: ContextDigest,
    pub logical_records: u64,
    pub prefix_records: u64,
    pub mapping_refs: u64,
    pub profile_binding: ContextDigest,
}
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "origin",
    content = "identity",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum WireOrigin {
    FreshOpening(FreshOpeningIdentity),
    RestoredBaseline(Box<RestoredBaselineIdentity>),
}

/// Decoded identity is not native approval and cannot initialize an execution origin.
/// ```compile_fail
/// use foundry_runtime::coordinated::origin::OriginIdentity;
/// fn approve(identity: OriginIdentity) { identity.accept_from_trusted_owner(); }
/// ```
pub struct OriginIdentity(WireOrigin);
impl OriginIdentity {
    pub fn fresh(identity: FreshOpeningIdentity) -> Result<Self, CaptureError> {
        Self::checked(WireOrigin::FreshOpening(identity))
    }
    pub fn restored(identity: RestoredBaselineIdentity) -> Result<Self, CaptureError> {
        Self::checked(WireOrigin::RestoredBaseline(Box::new(identity)))
    }
    fn checked(wire: WireOrigin) -> Result<Self, CaptureError> {
        let positive = |value, max| value > 0 && value <= max;
        match &wire {
            WireOrigin::FreshOpening(i) if !positive(i.prompt_bytes, MAX_PROMPT_BYTES as u64) => {
                return Err(CaptureError::Limit);
            }
            WireOrigin::RestoredBaseline(i)
                if !positive(i.prompt_bytes, MAX_PROMPT_BYTES as u64)
                    || !positive(i.generation, i64::MAX as u64)
                    || !positive(i.witness_bytes, MAX_WITNESS_BYTES)
                    || !positive(i.logical_records, MAX_LOGICAL_RECORDS)
                    || !positive(i.prefix_records, MAX_PREFIX_RECORDS)
                    || !positive(i.mapping_refs, MAX_MAPPING_REFS) =>
            {
                return Err(CaptureError::Limit);
            }
            _ => {}
        }
        Ok(Self(wire))
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, CaptureError> {
        if bytes.len() > MAX_ORIGIN_BYTES {
            return Err(CaptureError::Limit);
        }
        Self::checked(serde_json::from_slice(bytes).map_err(|_| CaptureError::Invalid)?)
    }
    pub fn encode(&self) -> Result<Vec<u8>, CaptureError> {
        // All identity strings/counts are already bounded; no payload/history arrays.
        let bytes = serde_json::to_vec(&self.0).map_err(|_| CaptureError::Invalid)?;
        if bytes.len() > MAX_ORIGIN_BYTES {
            return Err(CaptureError::Limit);
        }
        Ok(bytes)
    }
    pub fn binding_digest(&self) -> Result<ContextDigest, CaptureError> {
        let encoded = self.encode()?;
        let domain = b"foundry.capture.origin-identity.v1";
        let mut bytes = Vec::with_capacity(domain.len() + encoded.len() + 16);
        bytes.extend_from_slice(&(domain.len() as u64).to_be_bytes());
        bytes.extend_from_slice(domain);
        bytes.extend_from_slice(&(encoded.len() as u64).to_be_bytes());
        bytes.extend_from_slice(&encoded);
        Ok(ContextDigest::of(&bytes))
    }
    /// Comparison only; neither a matching digest nor this object is an authority token.
    pub fn check_binding(&self, expected: ContextDigest) -> Result<(), CaptureError> {
        if self.binding_digest()? != expected {
            return Err(CaptureError::Binding);
        }
        Ok(())
    }
    pub fn is_restored(&self) -> bool {
        matches!(self.0, WireOrigin::RestoredBaseline(_))
    }
    /// Historical prompt attribution is immutable, independent of a future attempt.
    pub fn check_prompt(
        &self,
        session: &str,
        original_attempt: &str,
        digest: ContextDigest,
        bytes: u64,
    ) -> Result<(), CaptureError> {
        let (s, a, d, n) = match &self.0 {
            WireOrigin::FreshOpening(i) => (
                &i.session_id,
                &i.delivery_attempt_id,
                i.prompt_digest,
                i.prompt_bytes,
            ),
            WireOrigin::RestoredBaseline(i) => (
                &i.session_id,
                &i.original_prompt_attempt_id,
                i.prompt_digest,
                i.prompt_bytes,
            ),
        };
        if s.as_str() != session || a.as_str() != original_attempt || d != digest || n != bytes {
            return Err(CaptureError::Binding);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "origin_tests.rs"]
mod tests;
