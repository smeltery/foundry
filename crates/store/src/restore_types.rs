use crate::{AttemptFence, CheckpointRef, ContextCheckpointId};

/// Trusted caller input, not native restoration proof.
pub struct BeginRestoreInput {
    pub request_id: String,
    pub checkpoint_id: ContextCheckpointId,
    pub expected_session_version: u64,
    pub configuration_digest: [u8; 32],
}

/// A loading lease credential. Never serialize or log it.
pub struct RestoreFence {
    pub(crate) id: String,
    pub(crate) session: String,
    pub(crate) token: String,
}
impl RestoreFence {
    pub fn restore_id(&self) -> &str {
        &self.id
    }
}

pub enum BeginRestoreResult {
    Started {
        fence: RestoreFence,
        checkpoint: Box<CheckpointRef>,
    },
    Existing(Box<RestoreRef>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestorePhase {
    Loading,
    Restored,
    Continuing,
    Closed,
    Failed,
    Cancelled,
    Finished,
    Expired,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreClose {
    Closed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreRef {
    pub id: String,
    pub session_id: String,
    pub checkpoint_id: ContextCheckpointId,
    pub configuration_digest: [u8; 32],
    pub expected_session_version: u64,
    pub phase: RestorePhase,
    pub continuation: Option<ContinueReceipt>,
}

/// Digests are computed by the trusted caller; the store does not inspect native commands.
pub struct ContinueInput {
    pub request_id: String,
    pub configuration_digest: [u8; 32],
    pub delivery_digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinueReceipt {
    pub restore_id: String,
    pub attempt_id: String,
    pub request_id: String,
    pub delivery_digest: [u8; 32],
    pub checkpoint_id: ContextCheckpointId,
    pub session_version: u64,
    pub admitted_work_revision: u64,
    pub message: Option<ContinueMessageBinding>,
}

/// Returned only once, after durable authorization. No replay after a lost response.
pub struct ContinuePermit(pub(crate) AttemptFence);
impl ContinuePermit {
    pub fn into_attempt_fence(self) -> AttemptFence {
        self.0
    }
}
pub enum ContinueDecision {
    Granted {
        permit: ContinuePermit,
        receipt: ContinueReceipt,
    },
    AlreadyAuthorized(ContinueReceipt),
}

/// Exact queued input selected by a trusted caller; not native validation proof.
pub struct ContinueMessageInput {
    pub authorization: ContinueInput,
    pub message_id: String,
    pub expected_text_digest: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinueMessageBinding {
    pub message_id: String,
    pub sequence: u64,
    pub text_digest: [u8; 32],
    pub text_bytes: u64,
}
/// Issued once after commit. Trusted callers must not duplicate delivery themselves.
pub struct ContinueMessagePermit(
    pub(crate) AttemptFence,
    pub(crate) foundry_contracts::Message,
);
impl ContinueMessagePermit {
    pub fn into_delivery(self) -> (AttemptFence, foundry_contracts::Message) {
        (self.0, self.1)
    }
}
pub enum ContinueMessageDecision {
    Granted {
        permit: ContinueMessagePermit,
        receipt: ContinueReceipt,
    },
    AlreadyAuthorized(ContinueReceipt),
}
