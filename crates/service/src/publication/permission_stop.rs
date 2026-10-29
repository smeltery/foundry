//! Nonpublication finalization consumes runtime cleanup, not native evidence.
use super::*;
use foundry_contracts::{Lifecycle, Outcome, Session};
use foundry_runtime::coordinated::CaptureActionRequired;

impl Publication {
    pub fn check_action_required(
        &self,
        fence: &AttemptFence,
        stop: &CaptureActionRequired,
    ) -> Result<()> {
        let expected_stage = if self.baseline.is_some() { 0 } else { 1 };
        if !self.capture.matches_action_required(stop)
            || stop.session_id() != fence.session_id
            || stop.attempt_id() != fence.attempt_id
            || self.stage != expected_stage
            || !self.persistence.idle()
            || self.evidence.is_some()
            || self.validation.is_some()
            || self.verified.is_some()
        {
            return Err(ServiceError::Coordination(
                "capture permission stop mismatch",
            ));
        }
        Ok(())
    }

    pub fn finish_action_required(
        &mut self,
        store: &mut Store,
        fence: &AttemptFence,
        stop: CaptureActionRequired,
    ) -> Result<Session> {
        self.admit_action_required(store, fence, stop)?
            .finish(store, fence)
    }

    fn admit_action_required(
        &self,
        store: &Store,
        fence: &AttemptFence,
        stop: CaptureActionRequired,
    ) -> Result<AdmittedActionRequired> {
        self.check_action_required(fence, &stop)?;
        self.live(store, fence)?;
        let gate = &self.control.signals.permissions;
        let generation = gate.generation().map_err(|_| terminal_unavailable())?;
        let admit = || {
            gate.consume(generation, || AdmittedActionRequired {
                _cleanup: stop.into_cleanup(),
            })
            .map_err(|_| terminal_unavailable())
        };
        // The gate checks local cancellation and its deadline while the current
        // configuration is locked. Successful consumption admits the fixed result.
        // No local or permission lock survives this bounded admission operation.
        let admitted = match &self.control.permissions {
            Some(permissions) => permissions.with_current_configuration(
                &self.configuration.profile.configuration_digest,
                admit,
            )?,
            None => admit()?,
        };
        self.control.signals.stop_admission();
        Ok(admitted)
    }
}

struct AdmittedActionRequired {
    _cleanup: foundry_runtime::coordinated::CleanupReceipt,
}
impl AdmittedActionRequired {
    fn finish(self, store: &mut Store, fence: &AttemptFence) -> Result<Session> {
        // SQL may wait for another writer. After terminal admission, only durable
        // cancellation and the Store fence arbitrate this result. Later local
        // cancellation, configuration revocation or conservative expiry may lose;
        // they cannot roll back an already committed terminal outcome.
        store
            .finish_attempt(
                fence,
                Lifecycle::Exit,
                Some(Outcome::ActionRequired),
                Some("permission_action_required".into()),
            )
            .map_err(Into::into)
    }
}
fn terminal_unavailable() -> ServiceError {
    ServiceError::Coordination("capture terminal admission unavailable")
}

#[cfg(test)]
mod tests;
