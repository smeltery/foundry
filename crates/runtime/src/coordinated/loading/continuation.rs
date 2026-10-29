//! Trusted asynchronous owner handoff. Channel replies are not native or Store proof.
mod admitted;
mod pending;
use super::*;
use crate::coordinated::origin::OriginIdentity;
pub use admitted::{AdmittedProcess, RestoredRun};
use foundry_contracts::{
    context::ContextId, context_observation::ObservationBinding, context_v2::ContinuationIdentity,
};
pub use pending::PendingContinuation;
use std::sync::{Arc, mpsc::SyncSender};

#[derive(Clone, PartialEq, Eq)]
pub struct ContinuationJob(uuid::Uuid);
/// Inert comparison data. Only the trusted service can validate native/durable facts.
pub struct ContinuationBinding {
    pub observation: ObservationBinding,
    pub instance_id: ContextReceipt,
    pub baseline: OriginIdentity,
    /// Selected native policy before authorization; distinct from historical storage.
    pub effective_configuration_digest: ContextDigest,
}
pub enum ContinuationRequest {
    Authorize {
        job: ContinuationJob,
        binding: Arc<ContinuationBinding>,
        tail_receipt_digest: ContextDigest,
    },
    Accept {
        job: ContinuationJob,
        scope: LoadingScope,
        identity: ContinuationIdentity,
        receipt: ContextReceipt,
    },
}
pub enum ContinuationReply {
    Granted {
        job: ContinuationJob,
        identity: ContinuationIdentity,
        text: String,
    },
    Activated {
        job: ContinuationJob,
    },
    Rejected {
        job: ContinuationJob,
    },
}
/// Required trusted service endpoint. Never connected to adapter-controlled input.
/// Granted requires native validation and consumed durable authorization; Activated
/// requires exact durable acceptance and a final live-owner activation check.
pub struct ContinuationOwner {
    pub requests: SyncSender<ContinuationRequest>,
    pub replies: Receiver<ContinuationReply>,
}
pub(super) struct BaselineCheck {
    checkpoint: ContextId,
    payload: ContextDigest,
    record: ContextDigest,
    witness: ContextDigest,
    configuration: ContextDigest,
}
impl BaselineCheck {
    pub(super) fn new(d: &RestoreDescriptor) -> Self {
        Self {
            checkpoint: d.checkpoint_id.clone(),
            payload: d.payload_digest,
            record: d.record_digest,
            witness: d.witness_digest,
            configuration: d.configuration_digest,
        }
    }
    fn check(&self, binding: &ContinuationBinding) -> Result<(), RuntimeError> {
        let i = binding
            .baseline
            .restored_identity()
            .ok_or(RuntimeError::Invalid("restored baseline required"))?;
        if i.checkpoint_id != self.checkpoint
            || i.payload_digest != self.payload
            || i.record_digest != self.record
            || i.witness_digest != self.witness
            || i.profile_binding != binding.observation.profile_digest
        {
            return Err(RuntimeError::Invalid("restored baseline binding mismatch"));
        }
        Ok(())
    }
}
impl LoadedProcess {
    pub fn continue_with(
        mut self,
        binding: ContinuationBinding,
        owner: ContinuationOwner,
        cancellation: &Receiver<CancelMode>,
    ) -> Result<PendingContinuation, LoadingError> {
        self.poll(cancellation)?;
        let checked = (|| {
            if self.purpose() != LoadingPurpose::PrepareContinuation
                || self.observation.is_some()
                || self.observed_binding.as_ref() != Some(&binding.observation)
                || self.instance.as_ref() != Some(&binding.instance_id)
            {
                return Err(RuntimeError::Invalid(
                    "continuation requires validated loading selection",
                ));
            }
            self.descriptor
                .as_ref()
                .ok_or(RuntimeError::Invalid("missing loading descriptor"))?
                .check(&binding)
        })();
        if let Err(error) = checked {
            return Err(LoadingError {
                cause: error.into(),
                cleanup: self.stop().err(),
            });
        }
        PendingContinuation::new(self, binding, owner)
    }
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, unix))]
mod capture_tests;
#[cfg(all(test, unix))]
mod support;
