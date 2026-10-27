//! Private context transport only. A valid frame is not a semantic checkpoint seal.
//! Payload-bearing types intentionally do not implement Debug or Serialize.
#[path = "context_codec.rs"]
mod codec;
#[path = "context_fields.rs"]
mod fields;
pub use codec::{decode_command, decode_event, encode_command, encode_event, is_context_frame};
pub use fields::{
    ContextCoverage, ContextDigest, ContextFormat, ContextId, ContextPayload, ContextReceipt,
    InputCoverage, ResultCoverage,
};
use serde::{Deserialize, Serialize};

pub const CAPTURE_CAPABILITY: &str = "context_capture_v1";
pub const EXPORT_CAPABILITY: &str = "context_export_v1";
pub const RESTORE_CAPABILITY: &str = "context_restore_v1";
pub const CONTINUE_CAPABILITY: &str = "context_continue_v1";
pub const MAX_PAYLOAD_BYTES: usize = 1_048_576;
pub const MAX_BASE64_BYTES: usize = 1_398_104;
pub const MAX_FORMAT_BYTES: usize = 16_384;
pub const MAX_COVERAGE_BYTES: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ContextError {
    #[error("invalid context frame")]
    Frame,
    #[error("invalid context schema")]
    Schema,
    #[error("context limit exceeded")]
    Limit,
    #[error("invalid context identifier")]
    Identifier,
    #[error("invalid context digest")]
    Digest,
    #[error("invalid context encoding")]
    Encoding,
    #[error("context integrity mismatch")]
    Integrity,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextFailure {
    Unsupported,
    Busy,
    InvalidPayload,
    InvalidState,
    Limit,
    Validation,
    OwnershipLost,
    Cancelled,
}

pub enum ContextCommand {
    CaptureBegin {
        attempt_id: ContextId,
        capture_id: ContextId,
        format_digest: ContextDigest,
        configuration_digest: ContextDigest,
        challenge: ContextReceipt,
    },
    Export {
        attempt_id: ContextId,
        capture_id: ContextId,
        format_digest: ContextDigest,
        cut_digest: ContextDigest,
        challenge: ContextReceipt,
    },
    Release {
        attempt_id: ContextId,
        operation_id: ContextId,
    },
    RestoreBegin {
        attempt_id: ContextId,
        restore_id: ContextId,
        checkpoint_id: ContextId,
        record_digest: ContextDigest,
        format: ContextFormat,
        configuration_digest: ContextDigest,
        payload_length: u64,
        payload_digest: ContextDigest,
    },
    RestorePayload {
        attempt_id: ContextId,
        restore_id: ContextId,
        payload: ContextPayload,
        receipt: ContextReceipt,
    },
    Continue {
        attempt_id: ContextId,
        restore_id: ContextId,
        continuation_id: ContextId,
        input_id: ContextId,
        input_digest: ContextDigest,
        text: String,
        receipt: ContextReceipt,
    },
    Failed {
        attempt_id: ContextId,
        operation_id: ContextId,
        code: ContextFailure,
    },
}

/// Private transport evidence cannot be sent to an ordinary serializer/logger.
/// ```compile_fail
/// use foundry_contracts::context::ContextEvent;
/// fn log_candidate(event: &ContextEvent) { println!("{event:?}"); }
/// ```
/// ```compile_fail
/// use foundry_contracts::context::ContextEvent;
/// fn persist_raw(event: &ContextEvent) { serde_json::to_string(event).unwrap(); }
/// ```
pub enum ContextEvent {
    CaptureReady {
        attempt_id: ContextId,
        capture_id: ContextId,
        format_digest: ContextDigest,
        configuration_digest: ContextDigest,
        challenge: ContextReceipt,
    },
    Candidate {
        attempt_id: ContextId,
        capture_id: ContextId,
        challenge: ContextReceipt,
        format: ContextFormat,
        coverage: ContextCoverage,
        payload: ContextPayload,
    },
    RestorePrepared {
        attempt_id: ContextId,
        restore_id: ContextId,
        format_digest: ContextDigest,
        configuration_digest: ContextDigest,
    },
    Restored {
        attempt_id: ContextId,
        restore_id: ContextId,
        payload_digest: ContextDigest,
        format_digest: ContextDigest,
        configuration_digest: ContextDigest,
        receipt: ContextReceipt,
    },
    ContinuationAccepted {
        attempt_id: ContextId,
        restore_id: ContextId,
        continuation_id: ContextId,
        input_id: ContextId,
        input_digest: ContextDigest,
        receipt: ContextReceipt,
    },
    Failed {
        attempt_id: ContextId,
        operation_id: ContextId,
        code: ContextFailure,
    },
}
impl ContextCommand {
    pub fn attempt_id(&self) -> &ContextId {
        match self {
            Self::CaptureBegin { attempt_id, .. }
            | Self::Export { attempt_id, .. }
            | Self::Release { attempt_id, .. }
            | Self::RestoreBegin { attempt_id, .. }
            | Self::RestorePayload { attempt_id, .. }
            | Self::Continue { attempt_id, .. }
            | Self::Failed { attempt_id, .. } => attempt_id,
        }
    }
}
impl ContextEvent {
    pub fn attempt_id(&self) -> &ContextId {
        match self {
            Self::CaptureReady { attempt_id, .. }
            | Self::Candidate { attempt_id, .. }
            | Self::RestorePrepared { attempt_id, .. }
            | Self::Restored { attempt_id, .. }
            | Self::ContinuationAccepted { attempt_id, .. }
            | Self::Failed { attempt_id, .. } => attempt_id,
        }
    }
}
