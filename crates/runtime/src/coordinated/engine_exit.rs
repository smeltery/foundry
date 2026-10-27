//! Consuming handoff authority exists only after common owned cleanup.
use super::*;

/// Owned process cleanup completed; this is not durable publication or native proof.
pub struct CleanupReceipt {
    pub(super) _private: (),
}
pub struct PreparedCapture {
    pub(super) verified: super::super::capture::VerifiedCapture,
    pub(super) cleanup: CleanupReceipt,
}
impl PreparedCapture {
    pub fn verified(&self) -> &super::super::capture::VerifiedCapture {
        &self.verified
    }
    pub fn into_owner_parts(self) -> (super::super::capture::VerifiedCapture, CleanupReceipt) {
        (self.verified, self.cleanup)
    }
}
pub(crate) enum EngineExit {
    Ordinary(RunResult),
    Prepared(Box<PreparedCapture>),
}
pub(super) enum Proposal {
    Ordinary(RunResult),
    Validated(Box<super::super::capture::VerifiedCapture>),
}
impl From<RunResult> for Proposal {
    fn from(value: RunResult) -> Self {
        Self::Ordinary(value)
    }
}
