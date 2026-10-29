//! Private trusted-owner messages, never adapter-deserialized authority.
use super::capture::{CaptureCut, CaptureValidationJob, VerifiedCapture};
use foundry_contracts::context::ContextId;
use std::sync::{
    Arc,
    mpsc::{Receiver, SyncSender, sync_channel},
};

pub(super) struct CaptureOwnerIdentity;

pub const CAPTURE_OWNER_CAPACITY: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CaptureJobId(pub(super) uuid::Uuid);
impl CaptureJobId {
    /// Read the exact generated identity without constructing new authority.
    pub fn as_uuid(&self) -> &uuid::Uuid {
        &self.0
    }

    /// Each owner exchange must use a fresh ID; never reuse the activation job.
    pub(crate) fn fresh() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct CaptureCorrelation {
    pub job: CaptureJobId,
    pub attempt_id: ContextId,
    pub capture_id: ContextId,
}

pub enum CaptureRequest {
    ActivateAdmission {
        correlation: CaptureCorrelation,
        message_ack: bool,
    },
    FreezeAdmission {
        correlation: CaptureCorrelation,
    },
    PrepareCut {
        correlation: CaptureCorrelation,
    },
    ValidateCandidate {
        correlation: CaptureCorrelation,
        job: Box<CaptureValidationJob>,
    },
}

pub enum CaptureRejection {
    Cancelled,
    OwnershipLost,
    Invalid,
    Storage,
    Limit,
    Unsupported,
    Validation,
    Conflict,
}

pub enum CaptureReply {
    AdmissionActivated {
        correlation: CaptureCorrelation,
    },
    AdmissionFrozen {
        correlation: CaptureCorrelation,
    },
    CutPrepared {
        correlation: CaptureCorrelation,
        cut: Box<CaptureCut>,
    },
    CandidateVerified {
        correlation: CaptureCorrelation,
        verified: Box<VerifiedCapture>,
    },
    Rejected {
        correlation: CaptureCorrelation,
        code: CaptureRejection,
    },
}

pub struct CaptureOwner {
    pub(super) identity: Arc<CaptureOwnerIdentity>,
    pub(super) requests: SyncSender<CaptureRequest>,
    pub(super) replies: Receiver<CaptureReply>,
}

/// The service owns its worker and must stop/join it after runtime cleanup.
/// Acknowledgment never certifies model startup or permits reopening a frozen gate.
pub struct CaptureOwnerEndpoint {
    identity: Arc<CaptureOwnerIdentity>,
    pub requests: Receiver<CaptureRequest>,
    pub replies: SyncSender<CaptureReply>,
}

/// Retains only exact capture-channel identity, not a sender or execution authority.
pub struct CaptureCancellationMatcher {
    identity: Arc<CaptureOwnerIdentity>,
}
impl CaptureCancellationMatcher {
    pub fn matches(&self, stop: &super::CaptureCancelled) -> bool {
        Arc::ptr_eq(&self.identity, &stop.owner_identity)
    }
}

impl CaptureOwnerEndpoint {
    pub fn cancellation_matcher(&self) -> CaptureCancellationMatcher {
        CaptureCancellationMatcher {
            identity: Arc::clone(&self.identity),
        }
    }

    /// Match cleanup authority to the exact channel that owned this capture.
    pub fn matches_cancelled(&self, stop: &super::CaptureCancelled) -> bool {
        Arc::ptr_eq(&self.identity, &stop.owner_identity)
    }
    pub fn matches_action_required(&self, stop: &super::CaptureActionRequired) -> bool {
        Arc::ptr_eq(&self.identity, &stop.owner_identity)
    }
}

impl CaptureOwner {
    pub fn channel() -> (Self, CaptureOwnerEndpoint) {
        let (requests, request_rx) = sync_channel(CAPTURE_OWNER_CAPACITY);
        let (reply_tx, replies) = sync_channel(CAPTURE_OWNER_CAPACITY);
        let identity = Arc::new(CaptureOwnerIdentity);
        (
            Self {
                identity: Arc::clone(&identity),
                requests,
                replies,
            },
            CaptureOwnerEndpoint {
                identity,
                requests: request_rx,
                replies: reply_tx,
            },
        )
    }
}
