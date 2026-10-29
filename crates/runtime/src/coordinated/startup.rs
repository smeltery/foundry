//! Capture startup and terminal phases for the trusted library entrypoint.
#[path = "startup_terminal.rs"]
mod terminal;
use super::capture::ProfileBinding;
use super::{capture_owner::*, tool::protocol};
use crate::{RuntimeError, process::Process};
use foundry_contracts::{
    AdapterCommand, MESSAGE_ACK_CAPABILITY, PROTOCOL_VERSION, Session, context::*,
};
use std::{
    sync::mpsc::{TryRecvError, TrySendError},
    time::{Duration, Instant},
};

pub(super) struct StartupPlan {
    pub profile: Box<ProfileBinding>,
    pub timeout: Duration,
}

#[derive(PartialEq, Eq)]
enum Phase {
    AwaitReady,
    PersistingReady,
    BeginPending,
    AwaitCaptureReady,
    StartPending,
    ActivationPending,
    Active,
}

pub(super) struct Startup {
    terminal: Option<terminal::Terminal>,
    restored_origin: Option<std::sync::Arc<super::capture::RestoredCaptureOrigin>>,
    plan: StartupPlan,
    pub trace: super::capture::DeliveredTrace,
    owner: CaptureOwner,
    correlation: CaptureCorrelation,
    challenge: ContextReceipt,
    phase: Phase,
    since: Instant,
    message_ack: bool,
    pending: Option<CaptureRequest>,
    format_digest: ContextDigest,
    configuration_digest: ContextDigest,
}

impl Startup {
    pub fn new(
        plan: StartupPlan,
        owner: CaptureOwner,
        attempt: &str,
    ) -> Result<Self, RuntimeError> {
        if plan.timeout.is_zero() || plan.timeout > Duration::from_secs(60) {
            return Err(RuntimeError::Invalid(
                "capture startup timeout requires 0–60 seconds",
            ));
        }
        let format_digest = plan
            .profile
            .wire_digest()
            .map_err(|_| RuntimeError::Invalid("invalid capture profile"))?;
        let configuration_digest = plan
            .profile
            .configuration_wire_digest()
            .map_err(|_| RuntimeError::Invalid("invalid capture configuration binding"))?;
        let id = |value: &str| {
            ContextId::new(value).map_err(|_| RuntimeError::Invalid("invalid capture identifier"))
        };
        Ok(Self {
            terminal: None,
            restored_origin: None,
            plan,
            trace: super::capture::DeliveredTrace::new(),
            owner,
            correlation: CaptureCorrelation {
                job: CaptureJobId::fresh(),
                attempt_id: id(attempt)?,
                capture_id: id(&uuid::Uuid::new_v4().to_string())?,
            },
            challenge: ContextReceipt::new(uuid::Uuid::new_v4().to_string())
                .map_err(|_| protocol("capture challenge unavailable"))?,
            phase: Phase::AwaitReady,
            since: Instant::now(),
            message_ack: false,
            pending: None,
            format_digest,
            configuration_digest,
        })
    }
    pub(super) fn restored(
        plan: StartupPlan,
        owner: CaptureOwner,
        origin: std::sync::Arc<super::capture::RestoredCaptureOrigin>,
        trace: super::capture::DeliveredTrace,
        message_ack: bool,
    ) -> Result<Self, RuntimeError> {
        origin
            .check_trace(&trace)
            .map_err(|_| protocol("restored trace binding mismatch"))?;
        let mut state = Self::new(plan, owner, origin.continuation.attempt_id.as_str())?;
        state.restored_origin = Some(origin);
        state.trace = trace;
        state.message_ack = message_ack;
        state.enter(Phase::Active);
        Ok(state)
    }
    fn enter(&mut self, phase: Phase) {
        self.phase = phase;
        self.since = Instant::now();
    }
    pub fn ready(&mut self, capabilities: &[String]) -> Result<(), RuntimeError> {
        if self.phase != Phase::AwaitReady
            || ![CAPTURE_CAPABILITY, EXPORT_CAPABILITY]
                .iter()
                .all(|required| capabilities.iter().any(|value| value == required))
        {
            return Err(protocol(
                "capture capabilities unavailable or repeated readiness",
            ));
        }
        self.message_ack = capabilities
            .iter()
            .any(|value| value == MESSAGE_ACK_CAPABILITY);
        self.enter(Phase::PersistingReady);
        Ok(())
    }
    pub fn persisted_ready(&mut self) -> Result<(), RuntimeError> {
        if self.phase != Phase::PersistingReady {
            return Err(protocol("unexpected ready persistence"));
        }
        self.enter(Phase::BeginPending);
        Ok(())
    }
    pub fn context(&mut self, event: ContextEvent) -> Result<(), RuntimeError> {
        if self.terminal.is_some() {
            return self.terminal_context(event);
        }
        let ContextEvent::CaptureReady {
            attempt_id,
            capture_id,
            format_digest,
            configuration_digest,
            challenge,
        } = event
        else {
            return Err(protocol("unexpected capture startup frame"));
        };
        if self.phase != Phase::AwaitCaptureReady
            || attempt_id != self.correlation.attempt_id
            || capture_id != self.correlation.capture_id
            || format_digest != self.format_digest
            || configuration_digest != self.configuration_digest
            || challenge != self.challenge
        {
            return Err(protocol("capture readiness binding mismatch"));
        }
        self.enter(Phase::StartPending);
        Ok(())
    }
    pub(super) fn check_permission_stop(
        &self,
    ) -> Result<std::sync::Arc<CaptureOwnerIdentity>, RuntimeError> {
        if !self.active() || self.pending.is_some() || self.terminal.is_some() {
            return Err(protocol("capture permission stop outside active phase"));
        }
        // Active execution is bounded by the run deadline, not the startup timer.
        match self.owner.replies.try_recv() {
            Err(TryRecvError::Empty) => Ok(std::sync::Arc::clone(&self.owner.identity)),
            _ => Err(protocol("late or disconnected capture owner reply")),
        }
    }
    pub fn active(&self) -> bool {
        self.phase == Phase::Active
    }
    pub fn waiting_activation(&self) -> bool {
        self.phase == Phase::ActivationPending
    }

    /// One bounded owner job; full queues retain its exact correlation without retry IDs.
    /// Caller rechecks cancellation before using the returned admission permission.
    pub fn poll(
        &mut self,
        process: &Process,
        session: &Session,
    ) -> Result<Option<bool>, RuntimeError> {
        if self.terminal.is_some() {
            self.poll_terminal(process, session)?;
            return Ok(None);
        }
        if let Some(enabled) = self.poll_owner()? {
            return Ok(Some(enabled));
        }
        match self.phase {
            Phase::BeginPending => {
                process.send_context(&ContextCommand::CaptureBegin {
                    attempt_id: self.correlation.attempt_id.clone(),
                    capture_id: self.correlation.capture_id.clone(),
                    format_digest: self.format_digest,
                    configuration_digest: self.configuration_digest,
                    challenge: self.challenge.clone(),
                })?;
                self.enter(Phase::AwaitCaptureReady);
            }
            Phase::StartPending => {
                let reservation = self
                    .trace
                    .reserve_prompt(
                        &session.id,
                        self.correlation.attempt_id.as_str(),
                        &session.prompt,
                    )
                    .map_err(|_| protocol("opening trace rejected"))?;
                process.send(&AdapterCommand::Start {
                    version: PROTOCOL_VERSION,
                    attempt_id: self.correlation.attempt_id.as_str().into(),
                    session: session.clone(),
                })?;
                reservation.commit_enqueued();
                self.pending = Some(CaptureRequest::ActivateAdmission {
                    correlation: self.correlation.clone(),
                    message_ack: self.message_ack,
                });
                self.enter(Phase::ActivationPending);
            }
            _ => {}
        }
        self.submit_activation()?;
        Ok(None)
    }
    fn poll_owner(&mut self) -> Result<Option<bool>, RuntimeError> {
        if self.phase != Phase::Active && self.since.elapsed() >= self.plan.timeout {
            return Err(protocol("capture startup deadline exceeded"));
        }
        match self.owner.replies.try_recv() {
            Ok(reply) => {
                if self.phase != Phase::ActivationPending || self.pending.is_some() {
                    return Err(protocol("unsolicited capture owner reply"));
                }
                let (correlation, accepted) = match reply {
                    CaptureReply::AdmissionActivated { correlation } => (correlation, true),
                    CaptureReply::Rejected { correlation, .. } => (correlation, false),
                    _ => return Err(protocol("unexpected capture owner reply phase")),
                };
                if correlation != self.correlation {
                    return Err(protocol("capture owner correlation mismatch"));
                }
                if !accepted {
                    return Err(protocol("capture owner rejected admission"));
                }
                match self.owner.replies.try_recv() {
                    Err(TryRecvError::Empty) => {}
                    _ => return Err(protocol("duplicate or disconnected capture owner reply")),
                }
                self.enter(Phase::Active);
                return Ok(Some(self.message_ack));
            }
            Err(TryRecvError::Disconnected) => {
                return Err(protocol("capture owner reply channel closed"));
            }
            Err(TryRecvError::Empty) => {}
        }
        Ok(None)
    }
    fn submit_activation(&mut self) -> Result<(), RuntimeError> {
        if let Some(request) = self.pending.take() {
            match self.owner.requests.try_send(request) {
                Ok(()) => {}
                Err(TrySendError::Full(request)) => self.pending = Some(request),
                Err(TrySendError::Disconnected(_)) => {
                    return Err(protocol("capture owner request channel closed"));
                }
            }
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[path = "startup_harness.rs"]
mod harness;
#[cfg(all(test, unix))]
#[path = "startup_tests.rs"]
mod tests;
#[cfg(all(test, unix))]
#[path = "capture_tool_tests.rs"]
mod tool_tests;

#[cfg(all(test, unix))]
#[path = "terminal_tests.rs"]
mod terminal_tests;

#[cfg(all(test, unix))]
#[path = "trace_integration_tests.rs"]
mod trace_integration_tests;

#[cfg(all(test, unix))]
#[path = "terminal_failure_tests.rs"]
mod terminal_failure_tests;

#[cfg(all(test, unix))]
#[path = "capture_stop_tests.rs"]
mod stop_tests;
