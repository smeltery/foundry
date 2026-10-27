use crate::{Result, conflict};
use serde_json::Value;
use std::fmt;

pub const MAX_CHECKPOINT_BYTES: usize = 1024 * 1024;
pub const MAX_CHECKPOINT_METADATA_BYTES: usize = 16 * 1024;
pub const MAX_CHECKPOINT_COVERAGE_BYTES: usize = 64 * 1024;

/// Durable context identity, distinct from runtime persistence-exchange IDs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextCheckpointId(pub(crate) String);
impl ContextCheckpointId {
    pub fn parse(value: &str) -> Result<Self> {
        let id = uuid::Uuid::parse_str(value).map_err(|_| conflict("invalid checkpoint ID"))?;
        Ok(Self(id.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for ContextCheckpointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Durable facts only. This cut makes no assertion about native context inclusion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointCut {
    pub work_revision: u64,
    /// Highest event sequence included, or zero when no events exist.
    pub event_seq: u64,
    pub event_digest: [u8; 32],
    pub input_digest: [u8; 32],
    pub tool_digest: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextFormat {
    pub format_id: String,
    pub format_version: u32,
    pub profile_id: String,
    pub binding_id: String,
    pub binding_version: String,
}

/// Sorted, unique identities claimed covered by the producer. Store checks durable
/// identity/state consistency, never whether opaque bytes actually contain them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverageManifest {
    pub message_ids: Vec<String>,
    pub tool_call_ids: Vec<String>,
}

/// Ordinary production input, NOT semantic proof. Deliberately no Debug/Serialize.
pub struct OpaqueCheckpointInput {
    pub request_id: String,
    pub expected_head: Option<ContextCheckpointId>,
    pub cut: CheckpointCut,
    pub format: ContextFormat,
    /// Caller-computed model/policy/tool-contract/workspace binding; preserved, not verified.
    pub configuration_digest: [u8; 32],
    pub coverage: CoverageManifest,
    pub payload: Vec<u8>,
    /// Stored annotation only; never used as execution/restore authorization.
    pub producer_validation: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointRef {
    pub id: ContextCheckpointId,
    pub session_id: String,
    pub source_attempt_id: String,
    pub parent_id: Option<ContextCheckpointId>,
    pub generation: u64,
    pub format: ContextFormat,
    pub cut: CheckpointCut,
    /// Caller-computed model/policy/tool-contract/workspace binding; preserved, not verified.
    pub configuration_digest: [u8; 32],
    pub payload_digest: [u8; 32],
    /// Exact request identity, including coverage and producer annotation.
    pub request_digest: [u8; 32],
    /// Verified stored record identity; integrity only, not native restore authority.
    pub record_digest: [u8; 32],
    pub payload_length: u64,
    pub committed_ms: i64,
}

/// Private session data. No Debug/Serialize implementation exposes the payload.
pub struct PrivateCheckpoint {
    pub(crate) reference: CheckpointRef,
    pub(crate) payload: Vec<u8>,
    pub(crate) coverage: CoverageManifest,
    pub(crate) producer_validation: Option<Value>,
}
impl PrivateCheckpoint {
    pub fn reference(&self) -> &CheckpointRef {
        &self.reference
    }
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
    pub fn coverage(&self) -> &CoverageManifest {
        &self.coverage
    }
    pub fn producer_validation(&self) -> Option<&Value> {
        self.producer_validation.as_ref()
    }
}
