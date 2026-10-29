//! Opt-in tool coordination. Persistence is supplied by a trusted execution owner.
//! Existing runtime entrypoints remain default-deny.
pub mod capture;
mod capture_cancel;
pub mod capture_owner;
mod capture_stop;
pub use capture_cancel::CaptureCancelled;
pub use capture_owner::CaptureCancellationMatcher;
mod engine;
mod fresh_capture;
pub mod loading;
pub mod origin;
pub use fresh_capture::{CaptureExit, FreshCaptureOptions, run_fresh_capture};
mod ingress;
mod policy;
mod shared_reply;
mod startup;
mod tool;
pub mod tool_permission;
pub use engine::{CaptureActionRequired, CleanupReceipt, PreparedCapture};
pub use foundry_contracts::permission::PermissionDisposition;
pub use shared_reply::SharedToolReply;
pub use tool_permission::{
    ToolAdmissionFailure, ToolDispatchAuthorization, ToolDispatchGrant, ToolPolicyIdentity,
};

use crate::{AdapterMessage, AdapterSpec, RunResult, RuntimeError};
use cap_std::fs::Dir;
use foundry_contracts::{AdapterEvent, CancelMode, Session};
use foundry_tools::host;
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, SyncSender},
    time::Duration,
};

pub const PERSISTENCE_CAPACITY: usize = 2;
pub const MAX_TOOL_CALLS: usize = 128;

/// Runtime-generated identity; never obtained from adapter JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CheckpointId(uuid::Uuid);
impl CheckpointId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolIntent {
    pub call_id: String,
    pub tool: String,
    pub input: host::Input,
}
#[derive(Debug)]
pub enum PersistenceRequest {
    Event {
        checkpoint: CheckpointId,
        event: AdapterEvent,
    },
    ClaimTool {
        checkpoint: CheckpointId,
        intent: ToolIntent,
    },
    FinishTool {
        checkpoint: CheckpointId,
        evidence: ToolEvidence,
    },
    FinishToolWithEvidence {
        checkpoint: CheckpointId,
        evidence: ToolEvidence,
    },
}
#[derive(Debug)]
pub enum ToolEvidence {
    Replied(host::Reply),
    SharedReply(SharedToolReply),
    FailedBeforeSpawn,
    Uncertain,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolDisposition {
    Success,
    NoEffectFailure,
    Uncertain,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceFailure {
    Invalid,
    Conflict,
    Storage,
    OwnershipLost,
}
pub enum PersistenceReply {
    EventCommitted {
        checkpoint: CheckpointId,
    },
    ToolAuthorized {
        checkpoint: CheckpointId,
        authorization: Box<dyn ToolDispatchAuthorization>,
    },
    ToolBlocked {
        checkpoint: CheckpointId,
        disposition: PermissionDisposition,
    },
    ToolClaimed {
        checkpoint: CheckpointId,
    },
    ToolFinished {
        checkpoint: CheckpointId,
        disposition: ToolDisposition,
    },
    ToolFinishedEvidence {
        checkpoint: CheckpointId,
        disposition: ToolDisposition,
        output_digest: Option<[u8; 32]>,
    },
    Rejected {
        checkpoint: CheckpointId,
        code: PersistenceFailure,
    },
}

impl std::fmt::Debug for PersistenceReply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::EventCommitted { .. } => "EventCommitted",
            Self::ToolClaimed { .. } => "ToolClaimed",
            Self::ToolAuthorized { .. } => "ToolAuthorized",
            Self::ToolBlocked { .. } => "ToolBlocked",
            Self::ToolFinished { .. } => "ToolFinished",
            Self::ToolFinishedEvidence { .. } => "ToolFinishedEvidence",
            Self::Rejected { .. } => "Rejected",
        })
    }
}

/// Use bounded channels of PERSISTENCE_CAPACITY. The service owns and joins its
/// persistence worker after runtime cleanup, including on cancellation/errors.
pub struct RuntimePersistence {
    pub requests: SyncSender<PersistenceRequest>,
    pub replies: Receiver<PersistenceReply>,
}

/// Trusted configuration, never deserialized from an adapter request.
pub struct TrustedToolPolicy {
    identity: ToolPolicyIdentity,
    authorization_required: bool,
    helper: PathBuf,
    directory: Dir,
    grants: Vec<host::Grant>,
    limits: host::HostLimits,
    timeout: Duration,
}

/// Returns completion only after terminal persistence acknowledgment and cleanup.
/// Cancellation revokes queued dispatch; the caller must subsequently drain/join
/// its persistence owner before finalizing a durable session. No SQL runs here.
pub fn run_adapter_coordinated(
    spec: &AdapterSpec,
    session: &Session,
    attempt_id: &str,
    cancellation: &Receiver<CancelMode>,
    messages: &Receiver<AdapterMessage>,
    persistence: RuntimePersistence,
    tools: Option<TrustedToolPolicy>,
) -> Result<RunResult, RuntimeError> {
    engine::run(
        spec,
        session,
        attempt_id,
        cancellation,
        messages,
        persistence,
        ingress::ExecutionOptions {
            mode: ingress::ExecutionMode::Legacy,
            policy: tools,
        },
    )
}
