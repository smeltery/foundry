//! Checked post-release cancellation data, never permission to execute or publish.
use super::*;
use foundry_contracts::Session;
use foundry_runtime::coordinated::{
    CaptureCancellationMatcher, CaptureCancelled, loading::continuation::ContinuationCancelled,
};

pub struct RestoredCancellation {
    pub(super) session: Session,
    pub(super) attempt: String,
    pub(super) message: String,
    pub(super) scope: LoadingScope,
    pub(super) capture: Option<CaptureCancellationMatcher>,
    pub(super) cleanup: CleanupObserver,
}
impl std::fmt::Debug for RestoredCancellation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RestoredCancellation")
    }
}
impl RestoredCancellation {
    pub fn session(&self) -> &Session {
        &self.session
    }
    pub fn message_id(&self) -> &str {
        &self.message
    }
    pub fn matches_transition(&self, stop: &ContinuationCancelled) -> bool {
        stop.scope() == &self.scope
            && stop
                .attempt_id()
                .is_some_and(|id| id.as_str() == self.attempt)
            && stop.matches_cleanup(&self.cleanup)
    }
    pub fn matches_capture(&self, stop: &CaptureCancelled) -> bool {
        self.capture
            .as_ref()
            .is_some_and(|matcher| matcher.matches(stop))
            && stop.session_id() == self.session.id
            && stop.attempt_id() == self.attempt
    }
    pub fn into_session(self) -> Session {
        self.session
    }
}

pub(super) fn cancelled_operation(result: &Result<()>) -> bool {
    matches!(
        result,
        Err(ServiceError::Coordination(
            "restoration cancelled" | "stored restoration cancellation"
        ))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unrelated_operation_errors_never_authorize_cancellation_settlement() {
        assert!(cancelled_operation(&Err(ServiceError::Coordination(
            "restoration cancelled"
        ))));
        assert!(cancelled_operation(&Err(ServiceError::Coordination(
            "stored restoration cancellation"
        ))));
        for message in [
            "native publication mapping rejected",
            "restore owner job failed",
            "native comparison worker failed",
            "restore owner stopped",
        ] {
            assert!(!cancelled_operation(&Err(ServiceError::Coordination(
                message
            ))));
        }
        assert!(!cancelled_operation(&Ok(())));
        assert!(!cancelled_operation(&Err(ServiceError::Invalid(
            "cancelled"
        ))));
    }
}
