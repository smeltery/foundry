use super::*;

/// Checked cleanup of the registered process; never durable acceptance authority.
#[must_use]
pub struct ContinuationCancelled {
    observer: CleanupObserver,
    scope: LoadingScope,
    attempt: Option<ContextId>,
    mode: CancelMode,
}
impl ContinuationCancelled {
    pub fn mode(&self) -> CancelMode {
        self.mode
    }
    pub fn scope(&self) -> &LoadingScope {
        &self.scope
    }
    pub fn attempt_id(&self) -> Option<&ContextId> {
        self.attempt.as_ref()
    }
    pub fn matches_cleanup(&self, observer: &CleanupObserver) -> bool {
        self.observer.same(observer) && observer.status() == CleanupStatus::Complete
    }
}
pub(super) fn observed(
    receiver: &Receiver<CancelMode>,
) -> Result<Option<CancelMode>, LoadingCause> {
    match receiver.try_recv() {
        Ok(mode) => Ok(Some(mode)),
        Err(TryRecvError::Empty) => Ok(None),
        Err(TryRecvError::Disconnected) => Err(LoadingCause::OwnerDisconnected),
    }
}
pub(super) fn check(
    receiver: &Receiver<CancelMode>,
    mode: &mut Option<CancelMode>,
) -> Result<(), LoadingCause> {
    if let Some(value) = observed(receiver)? {
        *mode = Some(value);
        return Err(LoadingCause::Cancelled);
    }
    Ok(())
}
pub(super) fn require(
    receiver: &Receiver<CancelMode>,
    prior: Option<CancelMode>,
) -> Result<CancelMode, LoadingError> {
    let mode = match prior {
        Some(mode) => Ok(Some(mode)),
        None => observed(receiver),
    }
    .map_err(|cause| LoadingError {
        cause,
        cleanup: None,
    })?;
    mode.ok_or(LoadingError {
        cause: RuntimeError::Invalid("cancellation not observed").into(),
        cleanup: None,
    })
}
pub(super) fn finish(
    loaded: &mut LoadedProcess,
    attempt: Option<ContextId>,
    mode: CancelMode,
) -> Result<ContinuationCancelled, LoadingError> {
    loaded.stop_cancelled().map_err(|error| LoadingError {
        cause: LoadingCause::Cancelled,
        cleanup: Some(error),
    })?;
    Ok(ContinuationCancelled {
        observer: loaded.cleanup_observer(),
        scope: loaded.scope.clone(),
        attempt,
        mode,
    })
}
