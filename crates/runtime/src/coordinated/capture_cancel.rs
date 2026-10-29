//! Cancellation cleanup authority, never native publication evidence.
use super::{CleanupReceipt, capture_owner::CaptureOwnerIdentity};
use foundry_contracts::CancelMode;
use std::sync::Arc;

/// Minted only by the owned supervisor after cleanup or before any spawn.
#[must_use = "submit cleanup to the capture owner before shutting it down"]
pub struct CaptureCancelled {
    pub(super) owner_identity: Arc<CaptureOwnerIdentity>,
    pub(super) session_id: String,
    pub(super) attempt_id: String,
    pub(super) mode: CancelMode,
    pub(super) forced: bool,
    pub(super) cleanup: CleanupReceipt,
}
impl CaptureCancelled {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }
    pub fn mode(&self) -> CancelMode {
        self.mode
    }
    pub fn forced(&self) -> bool {
        self.forced
    }
    pub fn into_cleanup(self) -> CleanupReceipt {
        self.cleanup
    }
}
