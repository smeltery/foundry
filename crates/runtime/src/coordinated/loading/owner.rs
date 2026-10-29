use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

#[derive(Debug, thiserror::Error)]
pub enum LoadingCause {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("loading cancelled")]
    Cancelled,
    #[error("loading owner disconnected")]
    OwnerDisconnected,
    #[error("loading phase deadline exceeded")]
    PhaseTimeout,
    #[error("retained loading deadline exceeded")]
    RetainedTimeout,
    #[error("observation rejected: {0:?}")]
    ObservationRejected(foundry_contracts::context_observation::ObservationFailure),
}
#[derive(Debug, thiserror::Error)]
#[error("{cause}; cleanup failure: {cleanup:?}")]
pub struct LoadingError {
    pub cause: LoadingCause,
    pub cleanup: Option<RuntimeError>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupStatus {
    Pending,
    Complete,
    Failed,
}
#[derive(Clone)]
pub struct CleanupObserver(Arc<AtomicU8>);
impl CleanupObserver {
    pub(crate) fn complete(&self, success: bool) {
        self.0.store(if success { 1 } else { 2 }, Ordering::Release);
    }
    pub fn status(&self) -> CleanupStatus {
        match self.0.load(Ordering::Acquire) {
            1 => CleanupStatus::Complete,
            2 => CleanupStatus::Failed,
            _ => CleanupStatus::Pending,
        }
    }
}
/// A live transport owner, not a native validation receipt or execution permit.
#[must_use = "retain the loaded process or explicitly close it"]
pub struct LoadedProcess {
    pub(super) process: Option<Process>,
    pub(super) events: Receiver<ContextOutput>,
    pub(super) scope: LoadingScope,
    pub(super) binding: ContextDigest,
    pub(super) deadline: Instant,
    pub(super) observer: CleanupObserver,
    purpose: LoadingPurpose,
    pub(super) directory: PathBuf,
    pub(super) descriptor: Option<super::continuation::BaselineCheck>,
    pub(super) message_ack: bool,
    pub(super) can_capture: bool,
    pub(super) observed_binding: Option<foundry_contracts::context_observation::ObservationBinding>,
    pub(super) can_observe: bool,
    pub(super) observation: Option<super::observation::ObservationJob>,
    pub(super) observation_started: bool,
    pub(super) instance: Option<ContextReceipt>,
}
impl LoadedProcess {
    pub(super) fn new(
        process: Process,
        events: Receiver<ContextOutput>,
        scope: LoadingScope,
        binding: ContextDigest,
        deadline: Instant,
        purpose: LoadingPurpose,
    ) -> Self {
        Self {
            process: Some(process),
            events,
            scope,
            binding,
            deadline,
            purpose,
            directory: PathBuf::new(),
            descriptor: None,
            message_ack: false,
            can_capture: false,
            observed_binding: None,
            observer: CleanupObserver(Arc::new(AtomicU8::new(0))),
            can_observe: false,
            observation: None,
            observation_started: false,
            instance: None,
        }
    }
    pub fn purpose(&self) -> LoadingPurpose {
        self.purpose
    }
    pub fn scope(&self) -> &LoadingScope {
        &self.scope
    }
    pub fn begin_binding(&self) -> ContextDigest {
        self.binding
    }
    pub fn process_id(&self) -> u32 {
        self.process.as_ref().map_or(0, Process::process_id)
    }
    pub fn cleanup_observer(&self) -> CleanupObserver {
        self.observer.clone()
    }
    /// The service must drive polling while its separate validator/lease monitor runs.
    pub fn poll(&mut self, cancellation: &Receiver<CancelMode>) -> Result<(), LoadingError> {
        let result = (|| {
            if self.process.is_none() {
                return Err(RuntimeError::Invalid("loading owner closed").into());
            }
            cancelled(cancellation)?;
            if Instant::now() >= self.deadline {
                return Err(LoadingCause::RetainedTimeout);
            }
            match self.events.try_recv() {
                Err(TryRecvError::Empty) => Ok(()),
                Err(TryRecvError::Disconnected) => Err(RuntimeError::Exited.into()),
                Ok(ContextOutput::Ordinary(WireEvent::Error(error))) => Err(error.into()),
                Ok(ContextOutput::Ordinary(WireEvent::Closed)) => Err(RuntimeError::Exited.into()),
                Ok(_) => Err(RuntimeError::Protocol("unexpected retained loading output").into()),
            }
        })();
        result.map_err(|cause| LoadingError {
            cause,
            cleanup: self.stop().err(),
        })
    }
    pub fn close(mut self) -> Result<(), RuntimeError> {
        self.stop()
    }
    pub(super) fn stop(&mut self) -> Result<(), RuntimeError> {
        let Some(mut process) = self.process.take() else {
            return if self.observer.status() == CleanupStatus::Failed {
                Err(RuntimeError::Protocol("loading cleanup previously failed"))
            } else {
                Ok(())
            };
        };
        let result = process.stop_context_checked(&self.events);
        self.observer
            .0
            .store(if result.is_ok() { 1 } else { 2 }, Ordering::Release);
        result
    }
}
impl Drop for LoadedProcess {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
