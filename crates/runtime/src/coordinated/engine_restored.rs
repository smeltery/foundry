//! Restore handoff into the existing supervisor, with no spawn or opening command.
use super::super::{capture::DeliveredTrace, loading::CleanupObserver};
use super::*;
use std::{cell::RefCell, collections::VecDeque};

pub(in crate::coordinated) struct RestoredParts {
    pub process: Process,
    pub events: Receiver<ContextOutput>,
    pub buffered: VecDeque<ContextOutput>,
    pub delta: DeliveredTrace,
    pub message_ack: bool,
    pub observer: CleanupObserver,
    pub tools: Option<TrustedToolPolicy>,
    pub capture: Option<super::super::startup::Startup>,
}
pub(in crate::coordinated) fn run_restored(
    spec: &AdapterSpec,
    session: &Session,
    cancellation: &Receiver<CancelMode>,
    messages: &Receiver<AdapterMessage>,
    persistence: RuntimePersistence,
    parts: RestoredParts,
) -> Result<(EngineExit, DeliveredTrace), RuntimeError> {
    let started = Instant::now();
    let attempt = session
        .active_attempt_id
        .as_deref()
        .ok_or(RuntimeError::Invalid("missing restored attempt"))?;
    let mut state = Coordinator {
        spec,
        session,
        attempt,
        persistence,
        policy: parts.tools,
        process: parts.process,
        ingress: Ingress::Restored {
            events: parts.events,
            buffered: RefCell::new(parts.buffered),
        },
        startup: parts.capture,
        restored: Some(parts.delta),
        stop: Arc::new(AtomicBool::new(false)),
        job: None,
        event: None,
        ready: true,
        delivery: Delivery::default(),
        seen: HashSet::new(),
    };
    state.delivery.enabled = parts.message_ack;
    let result = state.supervise(cancellation, messages);
    state.stop.store(true, Ordering::Release);
    if let Some(helper) = state.job.as_mut().and_then(|job| job.helper.as_mut()) {
        helper.request_stop();
    }
    let Ingress::Restored { events, buffered } = &state.ingress else {
        unreachable!()
    };
    let cleanup = state.process.stop_context_checked(events);
    let helper = state.job.as_mut().map_or(Ok(()), Job::cleanup);
    let drained = if buffered.borrow().is_empty() {
        Ok(())
    } else {
        Err(protocol("unconsumed restored output after cleanup"))
    };
    parts
        .observer
        .complete(cleanup.is_ok() && helper.is_ok() && drained.is_ok());
    cleanup?;
    helper?;
    drained?;
    let result = state.finish(result?, started, cancellation, messages)?;
    Ok((result, state.restored.take().unwrap()))
}
