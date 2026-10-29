use super::*;
use crate::coordinated::{RuntimePersistence, TrustedToolPolicy, capture::DeliveredTrace};
use crate::{AdapterMessage, RunResult};
use std::collections::VecDeque;

/// One-use handoff minted by exact Accepted plus a trusted owner activation reply.
/// It is not independently verifiable native or durable proof.
#[must_use = "run or explicitly close the admitted process"]
pub struct AdmittedProcess {
    pub(super) loaded: LoadedProcess,
    pub(super) binding: Arc<ContinuationBinding>,
    pub(super) identity: ContinuationIdentity,
    pub(super) delta: DeliveredTrace,
    pub(super) buffered: VecDeque<ContextOutput>,
}
pub struct RestoredRun {
    pub result: RunResult,
    pub binding: Arc<ContinuationBinding>,
    pub identity: ContinuationIdentity,
    pub delta: DeliveredTrace,
}
enum Handoff {
    Running(
        Box<(
            crate::coordinated::engine::RestoredParts,
            Arc<ContinuationBinding>,
            ContinuationIdentity,
        )>,
    ),
    Cancelled(Box<crate::coordinated::CaptureCancelled>),
}
impl AdmittedProcess {
    pub fn process_id(&self) -> u32 {
        self.loaded.process_id()
    }
    pub fn cleanup_observer(&self) -> CleanupObserver {
        self.loaded.cleanup_observer()
    }
    pub fn identity(&self) -> &ContinuationIdentity {
        &self.identity
    }
    pub fn close(self) -> Result<(), RuntimeError> {
        self.loaded.close()
    }
    pub fn close_cancelled(
        mut self,
        receiver: &Receiver<CancelMode>,
    ) -> Result<ContinuationCancelled, LoadingError> {
        let mode = cancellation::require(receiver, None)?;
        cancellation::finish(
            &mut self.loaded,
            Some(self.identity.attempt_id.clone()),
            mode,
        )
    }
    pub fn run_restored(
        self,
        spec: &AdapterSpec,
        session: &foundry_contracts::Session,
        cancellation: &Receiver<CancelMode>,
        messages: &Receiver<AdapterMessage>,
        persistence: RuntimePersistence,
        tools: Option<TrustedToolPolicy>,
    ) -> Result<RestoredRun, RuntimeError> {
        let Handoff::Running(parts) = self.into_parts(session, cancellation, tools, None)? else {
            return Err(RuntimeError::Invalid(
                "ordinary restored cancellation handoff",
            ));
        };
        let (parts, binding, identity) = *parts;
        let (exit, delta) = crate::coordinated::engine::run_restored(
            spec,
            session,
            cancellation,
            messages,
            persistence,
            parts,
        )?;
        let crate::coordinated::engine::EngineExit::Ordinary(result) = exit else {
            return Err(RuntimeError::Protocol(
                "unexpected ordinary restored capture",
            ));
        };
        Ok(RestoredRun {
            result,
            binding,
            identity,
            delta,
        })
    }
    pub fn run_restored_capture(
        self,
        spec: &AdapterSpec,
        session: &foundry_contracts::Session,
        cancellation: &Receiver<CancelMode>,
        messages: &Receiver<AdapterMessage>,
        persistence: RuntimePersistence,
        options: crate::coordinated::FreshCaptureOptions,
    ) -> Result<crate::coordinated::CaptureExit, RuntimeError> {
        let parts = match self.into_parts(session, cancellation, None, Some(options))? {
            Handoff::Running(parts) => parts.0,
            Handoff::Cancelled(stop) => {
                return Ok(crate::coordinated::CaptureExit::Cancelled(stop));
            }
        };
        let (exit, _) = crate::coordinated::engine::run_restored(
            spec,
            session,
            cancellation,
            messages,
            persistence,
            parts,
        )?;
        use crate::coordinated::{CaptureExit, engine::EngineExit};
        match exit {
            EngineExit::ActionRequired(stop) => Ok(CaptureExit::ActionRequired(stop)),
            EngineExit::Prepared(prepared) => Ok(CaptureExit::Prepared(prepared)),
            EngineExit::Cancelled(stop) => Ok(CaptureExit::Cancelled(stop)),
            EngineExit::Ordinary(_) => Err(RuntimeError::Protocol(
                "ordinary completion during restored capture",
            )),
        }
    }
    fn into_parts(
        mut self,
        session: &foundry_contracts::Session,
        cancellation: &Receiver<CancelMode>,
        tools: Option<TrustedToolPolicy>,
        capture: Option<crate::coordinated::FreshCaptureOptions>,
    ) -> Result<Handoff, RuntimeError> {
        let checked = (|| {
            if Instant::now() >= self.loaded.deadline
                || session.active_attempt_id.as_deref() != Some(self.identity.attempt_id.as_str())
                || self
                    .binding
                    .baseline
                    .restored_identity()
                    .map(|b| b.session_id.as_str())
                    != Some(session.id.as_str())
                || std::path::Path::new(&session.directory) != self.loaded.directory
            {
                return Err(RuntimeError::Invalid(
                    "restored execution selection mismatch",
                ));
            }
            if let Some(options) = &capture {
                let config = options
                    .profile
                    .configuration_wire_digest()
                    .map_err(|_| RuntimeError::Invalid("invalid restored capture configuration"))?;
                if self.binding.effective_configuration_digest != config {
                    return Err(RuntimeError::Invalid(
                        "restored capture configuration changed",
                    ));
                }
            }
            if capture.is_some() && !self.loaded.can_capture {
                return Err(RuntimeError::Protocol(
                    "restored capture capabilities unavailable",
                ));
            }
            Ok(())
        })();
        if let Err(error) = checked {
            self.loaded.stop()?;
            return Err(error);
        }
        let observed = match cancellation::observed(cancellation) {
            Ok(mode) => mode,
            Err(_) => {
                self.loaded.stop()?;
                return Err(RuntimeError::Protocol(
                    "restored cancellation owner disconnected",
                ));
            }
        };
        if let Some(mode) = observed {
            self.loaded.stop_cancelled()?;
            if let Some(options) = capture {
                return Ok(Handoff::Cancelled(Box::new(
                    crate::coordinated::CaptureCancelled {
                        owner_identity: Arc::clone(&options.owner.identity),
                        session_id: session.id.clone(),
                        attempt_id: self.identity.attempt_id.as_str().into(),
                        mode,
                        forced: true,
                        cleanup: crate::coordinated::CleanupReceipt { _private: () },
                    },
                )));
            }
            return Err(RuntimeError::Invalid("restored admission cancelled"));
        }
        let capturing = capture.is_some();
        let (capture, tools) = match capture {
            Some(options) => {
                let origin = Arc::new(crate::coordinated::capture::RestoredCaptureOrigin {
                    binding: Arc::clone(&self.binding),
                    continuation: self.identity.clone(),
                    historical_configuration_digest: self
                        .loaded
                        .descriptor
                        .as_ref()
                        .ok_or(RuntimeError::Invalid("missing historical configuration"))?
                        .configuration,
                });
                let startup = crate::coordinated::startup::Startup::restored(
                    crate::coordinated::startup::StartupPlan {
                        profile: Box::new(options.profile),
                        timeout: options.phase_timeout,
                    },
                    options.owner,
                    origin,
                    std::mem::take(&mut self.delta),
                    self.loaded.message_ack,
                );
                match startup {
                    Ok(startup) => (Some(startup), options.policy),
                    Err(error) => {
                        self.loaded.stop()?;
                        return Err(error);
                    }
                }
            }
            None => (None, tools),
        };
        if let Err(error) = self
            .loaded
            .process
            .as_mut()
            .ok_or(RuntimeError::Invalid("restored process closed"))?
            .admit_continuation(self.identity.attempt_id.as_str(), capturing)
        {
            self.loaded.stop()?;
            return Err(error);
        }
        let observer = self.loaded.cleanup_observer();
        let process = self
            .loaded
            .process
            .take()
            .ok_or(RuntimeError::Invalid("restored process closed"))?;
        let events = std::mem::replace(&mut self.loaded.events, std::sync::mpsc::channel().1);
        Ok(Handoff::Running(Box::new((
            crate::coordinated::engine::RestoredParts {
                process,
                events,
                buffered: self.buffered,
                delta: self.delta,
                message_ack: self.loaded.message_ack,
                observer,
                tools,
                capture,
            },
            self.binding,
            self.identity,
        ))))
    }
}
