//! Consuming handoff authority exists only after common owned cleanup.
pub use super::super::capture_stop::CaptureActionRequired;
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
    ActionRequired(Box<CaptureActionRequired>),
}
pub(super) enum Proposal {
    PermissionStop(PermissionDisposition),
    Ordinary(RunResult),
    Validated(Box<super::super::capture::VerifiedCapture>),
}
impl From<RunResult> for Proposal {
    fn from(value: RunResult) -> Self {
        Self::Ordinary(value)
    }
}

impl Coordinator<'_> {
    /// Called only after explicit owned process/helper cleanup succeeds.
    pub(super) fn finish(
        &self,
        result: Proposal,
        started: Instant,
        cancellation: &Receiver<CancelMode>,
        messages: &Receiver<AdapterMessage>,
    ) -> Result<EngineExit, RuntimeError> {
        match result {
            Proposal::PermissionStop(disposition) => {
                match cancellation.try_recv() {
                    Ok(mode) => {
                        return Ok(EngineExit::Ordinary(RunResult::Cancelled {
                            mode,
                            forced: true,
                        }));
                    }
                    Err(TryRecvError::Disconnected)
                        if self.startup.is_some()
                            && matches!(self.ingress, Ingress::Restored { .. }) =>
                    {
                        return Err(protocol("restored owner disconnected after cleanup"));
                    }
                    _ => {}
                }
                match self.persistence.replies.try_recv() {
                    Err(TryRecvError::Empty) => {}
                    _ => return Err(protocol("late or disconnected permission owner")),
                }
                match disposition {
                    PermissionDisposition::ActionRequired => {
                        if let Some(startup) = &self.startup {
                            if self.event.is_some() || self.job.is_some() {
                                return Err(protocol("capture permission stop has pending work"));
                            }
                            if started.elapsed() >= self.spec.run_timeout {
                                return Err(RuntimeError::RunTimeout);
                            }
                            if messages.try_recv().is_ok() {
                                return Err(protocol("queued instruction after capture cleanup"));
                            }
                            let owner_identity = startup.check_permission_stop()?;
                            return Ok(EngineExit::ActionRequired(Box::new(
                                CaptureActionRequired {
                                    owner_identity,
                                    session_id: self.session.id.clone(),
                                    attempt_id: self.attempt.into(),
                                    cleanup: CleanupReceipt { _private: () },
                                },
                            )));
                        }
                        Ok(EngineExit::Ordinary(RunResult::Completed {
                            outcome: foundry_contracts::Outcome::ActionRequired,
                            result: "permission_action_required".into(),
                        }))
                    }
                    PermissionDisposition::Unsupported => Err(protocol("permission_unsupported")),
                    _ => Err(protocol("invalid permission stop")),
                }
            }
            Proposal::Ordinary(result) => Ok(EngineExit::Ordinary(result)),
            Proposal::Validated(verified) => {
                match cancellation.try_recv() {
                    Ok(mode) => {
                        return Ok(EngineExit::Ordinary(RunResult::Cancelled {
                            mode,
                            forced: true,
                        }));
                    }
                    Err(TryRecvError::Disconnected)
                        if matches!(self.ingress, Ingress::Restored { .. }) =>
                    {
                        return Err(protocol("restored owner disconnected after cleanup"));
                    }
                    _ => {}
                }
                if started.elapsed() >= self.spec.run_timeout {
                    return Err(RuntimeError::RunTimeout);
                }
                match self.persistence.replies.try_recv() {
                    Err(TryRecvError::Empty) => {}
                    _ => return Err(protocol("late or disconnected persistence receipt")),
                }
                if messages.try_recv().is_ok() {
                    return Err(protocol("queued instruction after capture cleanup"));
                }
                self.startup
                    .as_ref()
                    .ok_or_else(|| protocol("missing capture owner"))?
                    .check_owner_drained()?;
                Ok(EngineExit::Prepared(Box::new(PreparedCapture {
                    verified: *verified,
                    cleanup: CleanupReceipt { _private: () },
                })))
            }
        }
    }
}
