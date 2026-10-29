//! Cleanup-only cancellation matching; no checkpoint publication.
use super::*;
use foundry_runtime::coordinated::{CaptureCancellationMatcher, CaptureCancelled};
impl Publication {
    pub(crate) fn cancellation_matcher(&self) -> CaptureCancellationMatcher {
        self.capture.cancellation_matcher()
    }
    pub(crate) fn check_cancelled(
        &self,
        fence: &AttemptFence,
        stop: &CaptureCancelled,
    ) -> Result<()> {
        if !self.capture.matches_cancelled(stop)
            || stop.session_id() != fence.session_id
            || stop.attempt_id() != fence.attempt_id
        {
            return Err(ServiceError::Coordination("capture cancellation mismatch"));
        }
        Ok(())
    }
    pub(crate) fn cancelled_workers_complete(&mut self) -> Result<bool> {
        self.cancel();
        if let Some(task) = self.validation.as_mut() {
            if !task.join_cancelled()? {
                return Ok(false);
            }
            self.validation.take();
        }
        Ok(true)
    }
}
