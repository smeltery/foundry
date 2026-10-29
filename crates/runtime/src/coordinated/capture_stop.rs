//! Cleanup authority for a permission stop, never checkpoint publication proof.
use super::{CleanupReceipt, capture_owner::CaptureOwnerIdentity};
use std::sync::Arc;

/// A consuming, owner-bound permission result minted after checked runtime cleanup.
#[must_use = "submit the proof to its capture owner for durable finalization"]
pub struct CaptureActionRequired {
    pub(super) owner_identity: Arc<CaptureOwnerIdentity>,
    pub(super) session_id: String,
    pub(super) attempt_id: String,
    pub(super) cleanup: CleanupReceipt,
}
impl CaptureActionRequired {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }
    pub fn into_cleanup(self) -> CleanupReceipt {
        self.cleanup
    }
}
