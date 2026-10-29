//! Ordered loading supervision. A successful handshake is not native validation.
pub mod continuation;
mod observation;
mod owner;
use crate::{
    AdapterSpec, RuntimeError,
    process::{
        ContextOutput, LoadingCommand, LoadingProcessSpec, LoadingSendError, Process, WireEvent,
    },
};
use foundry_contracts::{
    AdapterEvent, CancelMode,
    context::{ContextDigest, ContextPayload, ContextReceipt},
    context_v2::{self, LoadingFrame, LoadingScope, RestoreDescriptor},
};
pub use owner::{CleanupObserver, CleanupStatus, LoadedProcess, LoadingCause, LoadingError};
use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError},
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(5);
/// Immutable caller intent; inspection never becomes continuation authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadingPurpose {
    Inspect,
    PrepareContinuation,
}
pub struct LoadingLimits {
    pub phase_timeout: Duration,
    pub retained_timeout: Duration,
}
pub struct LoadingPlan {
    directory: PathBuf,
    scope: LoadingScope,
    begin: LoadingFrame,
    binding: ContextDigest,
    payload: ContextPayload,
    limits: LoadingLimits,
    required: Vec<String>,
    purpose: LoadingPurpose,
}
impl LoadingPlan {
    pub fn new(
        directory: PathBuf,
        scope: LoadingScope,
        descriptor: RestoreDescriptor,
        payload: ContextPayload,
        limits: LoadingLimits,
        purpose: LoadingPurpose,
        required_capabilities: Vec<String>,
    ) -> Result<Self, RuntimeError> {
        deadline(limits.phase_timeout)?;
        deadline(limits.retained_timeout)?;
        if !directory.is_absolute()
            || descriptor.payload_length != payload.as_bytes().len() as u64
            || descriptor.payload_digest != payload.digest()
        {
            return Err(RuntimeError::Invalid(
                "loading payload or directory mismatch",
            ));
        }
        if required_capabilities.len() > 32
            || required_capabilities.iter().any(|value| {
                value.is_empty() || value.len() > 128 || value.chars().any(char::is_control)
            })
        {
            return Err(RuntimeError::Invalid(
                "invalid loading capability requirements",
            ));
        }
        let binding = context_v2::restore_binding(&scope, &descriptor)
            .map_err(|_| RuntimeError::Invalid("invalid loading descriptor"))?;
        let begin = LoadingFrame::begin(scope.clone(), descriptor)
            .map_err(|_| RuntimeError::Invalid("invalid loading descriptor"))?;
        Ok(Self {
            directory,
            scope,
            begin,
            binding,
            payload,
            limits,
            required: required_capabilities,
            purpose,
        })
    }
}
fn deadline(duration: Duration) -> Result<Instant, RuntimeError> {
    if duration.is_zero() {
        return Err(RuntimeError::Invalid("loading deadline must be positive"));
    }
    Instant::now()
        .checked_add(duration)
        .ok_or(RuntimeError::Invalid("loading deadline overflow"))
}
pub fn load(
    spec: &AdapterSpec,
    plan: LoadingPlan,
    cancellation: &Receiver<CancelMode>,
) -> Result<LoadedProcess, LoadingError> {
    let start = || -> Result<LoadedProcess, LoadingCause> {
        let ready_deadline = deadline(spec.ready_timeout)?;
        cancelled(cancellation)?;
        let (process, events) = Process::spawn_loading(
            spec,
            LoadingProcessSpec {
                directory: &plan.directory,
                scope: plan.scope.clone(),
            },
        )?;
        Ok(LoadedProcess::new(
            process,
            events,
            plan.scope.clone(),
            plan.binding,
            ready_deadline,
            plan.purpose,
        ))
    };
    let mut loaded = start().map_err(|cause| LoadingError {
        cause,
        cleanup: None,
    })?;
    if let Err(cause) = handshake(&mut loaded, &plan, cancellation) {
        return Err(LoadingError {
            cause,
            cleanup: loaded.stop().err(),
        });
    }
    Ok(loaded)
}
fn handshake(
    loaded: &mut LoadedProcess,
    plan: &LoadingPlan,
    cancellation: &Receiver<CancelMode>,
) -> Result<(), LoadingCause> {
    let event = next(loaded, cancellation)?;
    let ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Ready { capabilities, .. })) = event
    else {
        return Err(RuntimeError::Protocol("loading expected readiness").into());
    };
    let advertised = |required| capabilities.iter().any(|value| value == required);
    if !advertised(context_v2::RESTORE_CAPABILITY)
        || (plan.purpose == LoadingPurpose::PrepareContinuation
            && !advertised(context_v2::CONTINUE_CAPABILITY))
        || plan
            .required
            .iter()
            .any(|required| !capabilities.contains(required))
    {
        return Err(RuntimeError::Protocol("required loading capability missing").into());
    }
    loaded.directory = plan.directory.clone();
    loaded.descriptor = plan
        .begin
        .descriptor()
        .map(continuation::BaselineCheck::new);
    loaded.message_ack = capabilities
        .iter()
        .any(|v| v == foundry_contracts::MESSAGE_ACK_CAPABILITY);
    loaded.can_capture = [
        foundry_contracts::context::CAPTURE_CAPABILITY,
        foundry_contracts::context::EXPORT_CAPABILITY,
    ]
    .iter()
    .all(|required| capabilities.iter().any(|v| v == required));
    loaded.can_observe = capabilities
        .iter()
        .any(|value| value == foundry_contracts::context_observation::OBSERVE_CAPABILITY);
    loaded.deadline = deadline(plan.limits.phase_timeout)?;
    send(loaded, &plan.begin, cancellation)?;
    let ContextOutput::Loading(prepared) = next(loaded, cancellation)? else {
        return Err(RuntimeError::Protocol("loading expected prepared").into());
    };
    prepared
        .check_prepared(&plan.scope, plan.binding)
        .map_err(|_| RuntimeError::Protocol("loading prepared binding mismatch"))?;
    let receipt = ContextReceipt::new(uuid::Uuid::new_v4().to_string())
        .map_err(|_| RuntimeError::Invalid("loading receipt unavailable"))?;
    let payload = LoadingFrame::payload(
        plan.scope.clone(),
        plan.binding,
        &plan.payload,
        receipt.clone(),
    )
    .map_err(|_| RuntimeError::Invalid("invalid loading payload"))?;
    loaded.deadline = deadline(plan.limits.phase_timeout)?;
    send(loaded, &payload, cancellation)?;
    let ContextOutput::Loading(restored) = next(loaded, cancellation)? else {
        return Err(RuntimeError::Protocol("loading expected restored").into());
    };
    restored
        .check_restored(&plan.scope, plan.binding, plan.payload.digest(), &receipt)
        .map_err(|_| RuntimeError::Protocol("loading restored binding mismatch"))?;
    cancelled(cancellation)?;
    loaded.deadline = deadline(plan.limits.retained_timeout)?;
    Ok(())
}
fn next(
    loaded: &LoadedProcess,
    cancellation: &Receiver<CancelMode>,
) -> Result<ContextOutput, LoadingCause> {
    loop {
        check(loaded, cancellation)?;
        let event = match loaded.events.recv_timeout(POLL) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Err(RuntimeError::Exited.into()),
        };
        check(loaded, cancellation)?;
        return match event {
            ContextOutput::Ordinary(WireEvent::Error(error)) => Err(error.into()),
            ContextOutput::Ordinary(WireEvent::Closed) => Err(RuntimeError::Exited.into()),
            event => Ok(event),
        };
    }
}
fn check(loaded: &LoadedProcess, cancellation: &Receiver<CancelMode>) -> Result<(), LoadingCause> {
    cancelled(cancellation)?;
    if Instant::now() >= loaded.deadline {
        return Err(LoadingCause::PhaseTimeout);
    }
    Ok(())
}
fn cancelled(cancellation: &Receiver<CancelMode>) -> Result<(), LoadingCause> {
    match cancellation.try_recv() {
        Ok(_) => Err(LoadingCause::Cancelled),
        Err(TryRecvError::Disconnected) => Err(LoadingCause::OwnerDisconnected),
        Err(TryRecvError::Empty) => Ok(()),
    }
}
fn send(
    loaded: &LoadedProcess,
    frame: &LoadingFrame,
    cancellation: &Receiver<CancelMode>,
) -> Result<(), LoadingCause> {
    let process = loaded
        .process
        .as_ref()
        .ok_or(RuntimeError::Invalid("loading owner closed"))?;
    let mut command: LoadingCommand = process.prepare_loading(frame)?;
    loop {
        check(loaded, cancellation)?;
        match process.try_send_loading(command) {
            Ok(()) => return Ok(()),
            Err(LoadingSendError::Disconnected) => return Err(RuntimeError::CommandQueue.into()),
            Err(LoadingSendError::Full(unsent)) => command = unsent,
        }
        std::thread::sleep(POLL);
    }
}
#[cfg(all(test, unix))]
#[path = "loading/tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "loading/observation_tests.rs"]
mod observation_tests;
