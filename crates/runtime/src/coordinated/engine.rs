#[path = "engine_events.rs"]
mod events;
#[path = "engine_exit.rs"]
mod exit;
use super::{
    ingress::{ExecutionOptions, Ingress},
    tool::{Job, protocol},
    *,
};
use crate::{
    delivery::Delivery,
    process::{ContextOutput, Process, WireEvent},
};
pub(super) use exit::EngineExit;
use exit::Proposal;
pub use exit::{CleanupReceipt, PreparedCapture};
use foundry_contracts::{AdapterCommand, MESSAGE_ACK_CAPABILITY, PROTOCOL_VERSION};
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{TryRecvError, TrySendError},
    },
    time::Instant,
};

struct EventJob {
    checkpoint: CheckpointId,
    pending: Option<PersistenceRequest>,
    action: Action,
}
enum Action {
    None,
    Ready(bool),
    Tool(host::Request),
    Denied(String, &'static str),
    Terminal(RunResult),
    Failed,
    MessageReceipt(String),
    CaptureTerminal,
}
struct Coordinator<'a> {
    spec: &'a AdapterSpec,
    session: &'a Session,
    attempt: &'a str,
    persistence: RuntimePersistence,
    policy: Option<TrustedToolPolicy>,
    process: Process,
    ingress: Ingress,
    startup: Option<super::startup::Startup>,
    stop: Arc<AtomicBool>,
    job: Option<Job>,
    event: Option<EventJob>,
    ready: bool,
    delivery: Delivery,
    seen: HashSet<String>,
}

pub(super) fn run(
    spec: &AdapterSpec,
    session: &Session,
    attempt: &str,
    cancellation: &Receiver<CancelMode>,
    messages: &Receiver<AdapterMessage>,
    persistence: RuntimePersistence,
    options: ExecutionOptions,
) -> Result<RunResult, RuntimeError> {
    match run_owned(
        spec,
        session,
        attempt,
        cancellation,
        messages,
        persistence,
        options,
    )? {
        EngineExit::Ordinary(result) => Ok(result),
        EngineExit::Prepared(prepared) => {
            drop(prepared);
            Err(RuntimeError::Invalid(
                "capture requires private finalization",
            ))
        }
    }
}
pub(super) fn run_owned(
    spec: &AdapterSpec,
    session: &Session,
    attempt: &str,
    cancellation: &Receiver<CancelMode>,
    messages: &Receiver<AdapterMessage>,
    persistence: RuntimePersistence,
    options: ExecutionOptions,
) -> Result<EngineExit, RuntimeError> {
    let started = Instant::now();
    if attempt.is_empty()
        || attempt.len() > 128
        || session.id.is_empty()
        || session.active_attempt_id.as_deref() != Some(attempt)
        || !std::path::Path::new(&session.directory).is_absolute()
    {
        return Err(RuntimeError::Invalid(
            "invalid coordinated attempt or working directory",
        ));
    }
    if let Ok(mode) = cancellation.try_recv() {
        return Ok(EngineExit::Ordinary(RunResult::Cancelled {
            mode,
            forced: false,
        }));
    }
    let (process, ingress, startup) = options.mode.spawn(spec, session)?;
    let mut state = Coordinator {
        spec,
        session,
        attempt,
        persistence,
        policy: options.policy,
        process,
        ingress,
        startup,
        stop: Arc::new(AtomicBool::new(false)),
        job: None,
        event: None,
        ready: false,
        delivery: Delivery::default(),
        seen: HashSet::new(),
    };
    let result = state.supervise(cancellation, messages);
    state.stop.store(true, Ordering::Release);
    if let Some(helper) = state.job.as_mut().and_then(|job| job.helper.as_mut()) {
        helper.request_stop();
    }
    let adapter_cleanup = if matches!(result, Ok(Proposal::Validated(_))) {
        match &state.ingress {
            Ingress::Context(events) => state.process.stop_context_checked(events),
            Ingress::Legacy => Err(protocol("capture cleanup requires private transport")),
        }
    } else {
        state.process.stop()
    };
    let helper_cleanup = state.job.as_mut().map_or(Ok(()), Job::cleanup);
    adapter_cleanup?;
    helper_cleanup?;
    match result? {
        Proposal::Ordinary(result) => Ok(EngineExit::Ordinary(result)),
        Proposal::Validated(verified) => {
            if let Ok(mode) = cancellation.try_recv() {
                return Ok(EngineExit::Ordinary(RunResult::Cancelled {
                    mode,
                    forced: true,
                }));
            }
            if started.elapsed() >= spec.run_timeout {
                return Err(RuntimeError::RunTimeout);
            }
            match state.persistence.replies.try_recv() {
                Err(TryRecvError::Empty) => {}
                _ => return Err(protocol("late or disconnected persistence receipt")),
            }
            if messages.try_recv().is_ok() {
                return Err(protocol("queued instruction after capture cleanup"));
            }
            state
                .startup
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

impl Coordinator<'_> {
    fn supervise(
        &mut self,
        cancellation: &Receiver<CancelMode>,
        messages: &Receiver<AdapterMessage>,
    ) -> Result<Proposal, RuntimeError> {
        let started = Instant::now();
        let mut stopping = None;
        let mut observed_control = None;
        loop {
            while let Some(mode) = observed_control
                .take()
                .or_else(|| cancellation.try_recv().ok())
            {
                self.stop.store(true, Ordering::Release);
                if let Some(helper) = self.job.as_mut().and_then(|job| job.helper.as_mut()) {
                    helper.request_stop();
                }
                if mode == CancelMode::Hard {
                    return Ok(RunResult::Cancelled { mode, forced: true }.into());
                }
                if stopping.is_none() {
                    stopping = Some(Instant::now());
                    if !self.ready {
                        return Ok(RunResult::Cancelled {
                            mode,
                            forced: false,
                        }
                        .into());
                    }
                    self.process.send(&AdapterCommand::Cancel {
                        attempt_id: self.attempt.into(),
                        mode,
                    })?;
                }
            }
            if let Some(since) = stopping {
                if since.elapsed() >= self.spec.cancel_timeout {
                    return Ok(RunResult::Cancelled {
                        mode: CancelMode::Graceful,
                        forced: true,
                    }
                    .into());
                }
                match self.ingress.try_recv(&self.process.events) {
                    Ok(ContextOutput::Ordinary(WireEvent::Closed))
                    | Err(TryRecvError::Disconnected) => {
                        return Ok(RunResult::Cancelled {
                            mode: CancelMode::Graceful,
                            forced: false,
                        }
                        .into());
                    }
                    Ok(ContextOutput::Ordinary(WireEvent::Event(
                        AdapterEvent::Settled { .. } | AdapterEvent::Failed { .. },
                    ))) => {
                        return Ok(RunResult::Cancelled {
                            mode: CancelMode::Graceful,
                            forced: false,
                        }
                        .into());
                    }
                    Ok(ContextOutput::Ordinary(WireEvent::Error(error))) => return Err(error),
                    _ => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                }
            }
            if !self.ready && started.elapsed() >= self.spec.ready_timeout {
                return Err(RuntimeError::ReadyTimeout);
            }
            if started.elapsed() >= self.spec.run_timeout {
                return Err(RuntimeError::RunTimeout);
            }
            if let Some(startup) = self.startup.as_mut() {
                let admitted = startup.poll(&self.process, self.session)?;
                if let Ok(mode) = cancellation.try_recv() {
                    observed_control = Some(mode);
                    continue;
                }
                if let Some(enabled) = admitted {
                    self.delivery.enabled = enabled;
                }
            }
            if let Some(job) = self.job.as_mut() {
                job.poll()?;
            }
            if let Some(event) = self.event.as_mut() {
                submit(&self.persistence, &mut event.pending)?;
            }
            if let Some(job) = self.job.as_mut() {
                submit(&self.persistence, &mut job.pending)?;
            }
            match self.persistence.replies.try_recv() {
                Ok(reply) => {
                    // Recheck before consuming authority, not just at the loop boundary.
                    if let Ok(mode) = cancellation.try_recv() {
                        observed_control = Some(mode);
                        continue;
                    }
                    if let Some(result) = self.persisted(reply)? {
                        return Ok(result.into());
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    return Err(protocol("persistence reply channel closed"));
                }
                Err(TryRecvError::Empty) => {}
            }
            let terminal = self.startup.as_ref().is_some_and(|s| s.terminal_pending());
            if terminal {
                if messages.try_recv().is_ok() {
                    return Err(protocol("queued instruction at capture cut"));
                }
                if self.job.is_some() {
                    return Err(protocol("outstanding capture tool work"));
                }
                if self.event.is_none() {
                    self.startup.as_mut().unwrap().advance_cut()?;
                }
            }
            if (self.event.is_none() || terminal)
                && !self
                    .startup
                    .as_ref()
                    .is_some_and(|startup| startup.waiting_activation())
            {
                self.delivery.forward_traced(
                    messages,
                    &self.process,
                    self.attempt,
                    self.startup.as_mut().map(|s| &mut s.trace),
                )?;
                match self.ingress.try_recv(&self.process.events) {
                    Ok(ContextOutput::Ordinary(WireEvent::Event(event))) => self.event(event)?,
                    Ok(ContextOutput::Loading(_)) => {
                        return Err(protocol("loading frame outside loading controller"));
                    }
                    Ok(ContextOutput::Context(event)) => {
                        self.startup
                            .as_mut()
                            .ok_or_else(|| protocol("unexpected context phase"))?
                            .context(event)?;
                    }
                    Ok(ContextOutput::Ordinary(WireEvent::Error(error))) => return Err(error),
                    Ok(ContextOutput::Ordinary(WireEvent::Closed))
                    | Err(TryRecvError::Disconnected) => {
                        return Err(RuntimeError::Exited);
                    }
                    Err(TryRecvError::Empty) => {
                        if let Ok(mode) = cancellation.try_recv() {
                            observed_control = Some(mode);
                            continue;
                        }
                        // Check already queued adapter events before releasing a durable
                        // result. Settlement observed with an outstanding job is invalid.
                        if let Some(job) = self.job.as_mut()
                            && job.deliver(
                                &self.process,
                                self.attempt,
                                self.startup.as_mut().map(|s| &mut s.trace),
                            )?
                        {
                            self.job.take();
                        }
                    }
                }
            }
            if terminal
                && self.event.is_none()
                && let Some(verified) = self.startup.as_mut().unwrap().take_verified()
            {
                if let Ok(mode) = cancellation.try_recv() {
                    return Ok(RunResult::Cancelled {
                        mode,
                        forced: false,
                    }
                    .into());
                }
                self.startup.as_ref().unwrap().check_owner_drained()?;
                return Ok(Proposal::Validated(Box::new(verified)));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn submit(
    persistence: &RuntimePersistence,
    pending: &mut Option<PersistenceRequest>,
) -> Result<(), RuntimeError> {
    if let Some(request) = pending.take() {
        match persistence.requests.try_send(request) {
            Ok(()) => {}
            Err(TrySendError::Full(request)) => *pending = Some(request),
            Err(TrySendError::Disconnected(_)) => {
                return Err(protocol("persistence request channel closed"));
            }
        }
    }
    Ok(())
}
