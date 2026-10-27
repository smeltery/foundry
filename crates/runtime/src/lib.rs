//! Synchronous, isolated subprocess supervision. No model or tool execution is built in.

pub mod coordinated;
mod delivery;
mod pipe_workers;
mod process;
pub mod tool_process;
pub use delivery::{AdapterMessage, MAX_MESSAGES_PER_ATTEMPT};

use foundry_contracts::{
    AdapterCommand, AdapterEvent, CancelMode, MESSAGE_ACK_CAPABILITY, Outcome, PROTOCOL_VERSION,
    Session,
};
use process::{Process, WireEvent};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Adapter command and bounded lifecycle settings. The executable is resolved
/// before changing to the session directory; arguments never pass through a shell.
#[derive(Debug, Clone)]
pub struct AdapterSpec {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub ready_timeout: Duration,
    pub run_timeout: Duration,
    pub cancel_timeout: Duration,
}

impl AdapterSpec {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            args: Vec::new(),
            ready_timeout: Duration::from_secs(10),
            run_timeout: Duration::from_secs(3600),
            cancel_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunResult {
    Completed { outcome: Outcome, result: String },
    Cancelled { mode: CancelMode, forced: bool },
}

/// Diagnostics intentionally omit adapter stderr, frame contents and failure text.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("adapter I/O failure ({0:?})")]
    Io(std::io::ErrorKind),
    #[error("invalid runtime request: {0}")]
    Invalid(&'static str),
    #[error("adapter protocol failure: {0}")]
    Protocol(&'static str),
    #[error("adapter frame exceeds 2 MiB")]
    FrameTooLarge,
    #[error("adapter did not become ready before the deadline")]
    ReadyTimeout,
    #[error("adapter exceeded its execution deadline")]
    RunTimeout,
    #[error("adapter exited or closed output before settlement")]
    Exited,
    #[error("adapter reported failure (details withheld)")]
    AdapterFailed,
    #[error("adapter command queue is unavailable")]
    CommandQueue,
    #[error("owned process groups are unsupported on this platform")]
    UnsupportedPlatform,
}

impl From<std::io::Error> for RuntimeError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.kind())
    }
}

/// Run one attempt. The callback receives validated events, including denied
/// tool requests, but never settlement/failure events after observed cancellation.
/// Callbacks must be brief: this synchronous supervisor cannot interrupt them.
/// A disconnected cancellation channel does not cancel the attempt.
/// Every return after spawn stops/reaps the owned group and joins its pipe workers,
/// or reports cleanup failure. Parent pipe I/O and queue waits are interruptible;
/// OS scheduling and child reaping are not hard real-time guarantees. Descendants
/// that detach from the owned group are not contained, but their inherited pipes
/// cannot keep our workers waiting for EOF during cleanup.
pub fn run_adapter(
    spec: &AdapterSpec,
    session: &Session,
    attempt_id: &str,
    cancellation: &Receiver<CancelMode>,
    on_event: impl FnMut(&AdapterEvent),
) -> Result<RunResult, RuntimeError> {
    let (_sender, messages) = std::sync::mpsc::channel();
    run_adapter_with_messages(spec, session, attempt_id, cancellation, &messages, on_event)
}

/// Additive message transport. Messages are forwarded only after capability negotiation.
/// The caller must durably claim messages before placing them in this channel.
pub fn run_adapter_with_messages(
    spec: &AdapterSpec,
    session: &Session,
    attempt_id: &str,
    cancellation: &Receiver<CancelMode>,
    messages: &Receiver<AdapterMessage>,
    mut on_event: impl FnMut(&AdapterEvent),
) -> Result<RunResult, RuntimeError> {
    if attempt_id.is_empty() || session.id.is_empty() {
        return Err(RuntimeError::Invalid(
            "session and attempt IDs are required",
        ));
    }
    if session.active_attempt_id.as_deref() != Some(attempt_id) {
        return Err(RuntimeError::Invalid("active attempt does not match"));
    }
    if !std::path::Path::new(&session.directory).is_absolute() {
        return Err(RuntimeError::Invalid("working directory must be absolute"));
    }
    if let Ok(mode) = cancellation.try_recv() {
        return Ok(RunResult::Cancelled {
            mode,
            forced: false,
        });
    }
    let mut process = Process::spawn(spec, session)?;
    let result = supervise(
        spec,
        session,
        attempt_id,
        cancellation,
        messages,
        &mut on_event,
        &mut process,
    );
    // Even a settled adapter may leave children behind. Reap the owned group on
    // every path; a successful event does not transfer process ownership.
    process.stop()?;
    result
}

fn supervise(
    spec: &AdapterSpec,
    session: &Session,
    attempt_id: &str,
    cancellation: &Receiver<CancelMode>,
    messages: &Receiver<AdapterMessage>,
    on_event: &mut impl FnMut(&AdapterEvent),
    process: &mut Process,
) -> Result<RunResult, RuntimeError> {
    let started = Instant::now();
    let mut ready = false;
    let mut delivery = delivery::Delivery::default();
    let mut stopping: Option<(CancelMode, Instant)> = None;
    loop {
        while let Ok(mode) = cancellation.try_recv() {
            if mode == CancelMode::Hard {
                return Ok(RunResult::Cancelled { mode, forced: true });
            }
            if stopping.is_none() {
                stopping = Some((mode, Instant::now()));
                if ready {
                    process.send(&AdapterCommand::Cancel {
                        attempt_id: attempt_id.into(),
                        mode,
                    })?;
                }
            }
        }
        if let Some((mode, since)) = stopping {
            if since.elapsed() >= spec.cancel_timeout {
                return Ok(RunResult::Cancelled { mode, forced: true });
            }
        } else if !ready && started.elapsed() >= spec.ready_timeout {
            return Err(RuntimeError::ReadyTimeout);
        } else if started.elapsed() >= spec.run_timeout {
            return Err(RuntimeError::RunTimeout);
        }
        if stopping.is_none() {
            delivery.forward(messages, process, attempt_id)?;
        }
        let wire = match process.events.recv_timeout(Duration::from_millis(10)) {
            Ok(wire) => wire,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => WireEvent::Closed,
        };
        // Observe cancellation again before delivering a concurrently queued event.
        while let Ok(mode) = cancellation.try_recv() {
            if mode == CancelMode::Hard {
                return Ok(RunResult::Cancelled { mode, forced: true });
            }
            if stopping.is_none() {
                stopping = Some((mode, Instant::now()));
                if ready {
                    process.send(&AdapterCommand::Cancel {
                        attempt_id: attempt_id.into(),
                        mode,
                    })?;
                }
            }
        }
        let event = match wire {
            WireEvent::Event(event) => event,
            WireEvent::Closed => {
                return stopping.map_or(Err(RuntimeError::Exited), |(mode, _)| {
                    Ok(RunResult::Cancelled {
                        mode,
                        forced: false,
                    })
                });
            }
            WireEvent::Error(error) => return Err(error),
        };
        if let AdapterEvent::Ready {
            version,
            capabilities,
        } = &event
        {
            if ready || *version != PROTOCOL_VERSION {
                return Err(RuntimeError::Protocol(
                    "duplicate ready or unsupported version",
                ));
            }
            ready = true;
            delivery.enabled = capabilities
                .iter()
                .any(|value| value == MESSAGE_ACK_CAPABILITY);
            if let Some((mode, _)) = stopping {
                return Ok(RunResult::Cancelled {
                    mode,
                    forced: false,
                });
            }
            process.send(&AdapterCommand::Start {
                version: PROTOCOL_VERSION,
                attempt_id: attempt_id.into(),
                session: session.clone(),
            })?;
            on_event(&event);
            continue;
        }
        if !ready {
            return Err(RuntimeError::Protocol("event received before ready"));
        }
        let event_attempt = match &event {
            AdapterEvent::Text { attempt_id, .. }
            | AdapterEvent::ToolCall { attempt_id, .. }
            | AdapterEvent::Settled { attempt_id, .. }
            | AdapterEvent::Failed { attempt_id, .. }
            | AdapterEvent::MessageAccepted { attempt_id, .. } => attempt_id,
            AdapterEvent::Ready { .. } => unreachable!(),
        };
        if event_attempt != attempt_id {
            return Err(RuntimeError::Protocol("event belongs to another attempt"));
        }
        if let AdapterEvent::MessageAccepted { message_id, .. } = &event {
            delivery.validate_ack(message_id)?;
        }
        if let Some((mode, _)) = stopping {
            if matches!(
                event,
                AdapterEvent::Settled { .. } | AdapterEvent::Failed { .. }
            ) {
                return Ok(RunResult::Cancelled {
                    mode,
                    forced: false,
                });
            }
            continue;
        }
        match &event {
            AdapterEvent::ToolCall { call_id, .. } => process.send(&AdapterCommand::ToolResult {
                attempt_id: attempt_id.into(),
                call_id: call_id.clone(),
                result: serde_json::json!({"status": "denied", "code": "tool_execution_disabled"}),
            })?,
            AdapterEvent::Settled { outcome, result, .. } => {
                on_event(&event);
                return Ok(RunResult::Completed { outcome: *outcome, result: result.clone() });
            }
            AdapterEvent::Failed { .. } => {
                // Persistable failure evidence must never include adapter-supplied diagnostics.
                on_event(&AdapterEvent::Failed {
                    attempt_id: attempt_id.into(),
                    code: "adapter_failed".into(),
                    message: "Adapter reported failure; details withheld".into(),
                });
                return Err(RuntimeError::AdapterFailed);
            }
            _ => {}
        }
        on_event(&event);
    }
}
