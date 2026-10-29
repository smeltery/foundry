#[path = "engine_events.rs"]
mod events;
#[path = "engine_exit.rs"]
mod exit;
#[path = "engine_restored.rs"]
mod restored;
#[path = "engine_supervise.rs"]
mod supervise;
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
pub(super) use restored::{RestoredParts, run_restored};
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
    restored: Option<super::capture::DeliveredTrace>,
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
        restored: None,
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
            Ingress::Context(events) | Ingress::Restored { events, .. } => {
                state.process.stop_context_checked(events)
            }
            Ingress::Legacy => Err(protocol("capture cleanup requires private transport")),
        }
    } else {
        state.process.stop()
    };
    let helper_cleanup = state.job.as_mut().map_or(Ok(()), Job::cleanup);
    adapter_cleanup?;
    helper_cleanup?;
    state.finish(result?, started, cancellation, messages)
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
