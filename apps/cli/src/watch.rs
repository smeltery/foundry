//! Follow durable event revisions without owning or changing execution attempts.
use foundry_contracts::{Event, Lifecycle, Outcome, Session};
use foundry_store::{Store, StoreError};
use serde_json::json;
use std::{
    fmt,
    io::{self, Write},
    path::Path,
    sync::atomic::{AtomicU8, Ordering},
    thread,
    time::{Duration, Instant},
};

const PAGE_SIZE: u32 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchFormat {
    Text,
    JsonLines,
}

#[derive(Debug, Clone)]
pub struct WatchOptions {
    pub interval: Duration,
    pub format: WatchFormat,
    /// Exclusive durable sequence cursor. Revisions of an event have new sequences.
    pub after_sequence: u64,
    /// Optional caller deadline; production following has no default time limit.
    /// This does not interrupt blocking database or output operations.
    pub timeout: Option<Duration>,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            format: WatchFormat::Text,
            after_sequence: 0,
            timeout: None,
        }
    }
}

#[derive(Debug)]
pub enum WatchError {
    Invalid(String),
    DatabasePath(io::Error),
    Storage(StoreError),
    Output(io::Error),
    TimedOut,
}

impl fmt::Display for WatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid watch request: {message}"),
            Self::DatabasePath(error) => write!(formatter, "watch database path: {error}"),
            Self::Storage(error) => write!(formatter, "watch storage: {error}"),
            Self::Output(error) => write!(formatter, "watch output: {error}"),
            Self::TimedOut => write!(formatter, "watch deadline reached before completion"),
        }
    }
}

impl std::error::Error for WatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DatabasePath(error) | Self::Output(error) => Some(error),
            Self::Storage(error) => Some(error),
            _ => None,
        }
    }
}

impl From<StoreError> for WatchError {
    fn from(error: StoreError) -> Self {
        Self::Storage(error)
    }
}

/// Replay events after the cursor, then poll until an observed terminal session.
/// Each durable sequence is emitted once per invocation, including newer revisions
/// of an existing event. Writes are flushed before advancing the in-memory cursor.
/// Text progress uses stderr; text results and all JSONL records use stdout.
/// Returns 0/success, 1/failure, 3/action required, 2/error, or 130/suspended.
/// Errors are returned to the caller without hiding database or output failures.
pub fn watch(
    database: impl AsRef<Path>,
    session_id: &str,
    options: WatchOptions,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<u8, WatchError> {
    watch_with_control(
        database,
        session_id,
        options,
        &AtomicU8::new(0),
        stdout,
        stderr,
    )
}

/// Interrupt only this observer, returning 130 without cancelling its session.
/// Polling sleeps are interruptible; blocking database/output operations are not.
pub fn watch_with_control(
    database: impl AsRef<Path>,
    session_id: &str,
    options: WatchOptions,
    control: &AtomicU8,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<u8, WatchError> {
    if session_id.trim().is_empty() || options.interval.is_zero() {
        return Err(WatchError::Invalid(
            "session id and positive polling interval are required".into(),
        ));
    }
    if options.timeout.is_some_and(|timeout| timeout.is_zero())
        || options.after_sequence > i64::MAX as u64
    {
        return Err(WatchError::Invalid(
            "timeout must be positive and cursor must fit the durable range".into(),
        ));
    }
    let started = Instant::now();
    if control.load(Ordering::Acquire) != 0 {
        return Ok(130);
    }
    let database = database.as_ref();
    if !database
        .metadata()
        .map_err(WatchError::DatabasePath)?
        .is_file()
    {
        return Err(WatchError::Invalid(
            "database must be an existing file".into(),
        ));
    }
    let store = Store::open(database)?;
    let mut cursor = options.after_sequence;
    loop {
        if control.load(Ordering::Acquire) != 0 {
            return Ok(130);
        }
        check_deadline(started, options.timeout)?;
        // Read lifecycle before events: seeing a terminal state guarantees its
        // previously committed events are drained before the result is rendered.
        let before = store.get(session_id)?;
        let events = store.events(session_id, cursor, PAGE_SIZE)?;
        if !events.is_empty() {
            for event in events {
                if control.load(Ordering::Acquire) != 0 {
                    return Ok(130);
                }
                check_deadline(started, options.timeout)?;
                emit_event(&event, options.format, stdout, stderr)?;
                cursor = event.sequence;
            }
            continue;
        }
        if !matches!(before.lifecycle, Lifecycle::Queued | Lifecycle::Running) {
            let current = store.get(session_id)?;
            // A concurrent continuation invalidates the old terminal observation.
            if current.version != before.version {
                continue;
            }
            check_deadline(started, options.timeout)?;
            if control.load(Ordering::Acquire) != 0 {
                return Ok(130);
            }
            return emit_result(&current, cursor, options.format, stdout);
        }
        let delay = match options.timeout {
            Some(timeout) => options
                .interval
                .min(timeout.saturating_sub(started.elapsed())),
            None => options.interval,
        };
        let sleeping = Instant::now();
        while sleeping.elapsed() < delay {
            if control.load(Ordering::Acquire) != 0 {
                return Ok(130);
            }
            thread::sleep(
                delay
                    .saturating_sub(sleeping.elapsed())
                    .min(Duration::from_millis(25)),
            );
        }
    }
}

fn check_deadline(started: Instant, timeout: Option<Duration>) -> Result<(), WatchError> {
    if timeout.is_some_and(|timeout| started.elapsed() >= timeout) {
        Err(WatchError::TimedOut)
    } else {
        Ok(())
    }
}

fn emit_event(
    event: &Event,
    format: WatchFormat,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<(), WatchError> {
    match format {
        WatchFormat::JsonLines => writeln!(stdout, "{}", json!({"type":"event","event":event}))
            .and_then(|()| stdout.flush()),
        WatchFormat::Text => {
            let data = event.data.get("text").and_then(|value| value.as_str());
            let rendered = data
                .map(str::to_owned)
                .unwrap_or_else(|| event.data.to_string());
            writeln!(stderr, "[{}] {}: {}", event.sequence, event.kind, rendered)
                .and_then(|()| stderr.flush())
        }
    }
    .map_err(WatchError::Output)
}

fn emit_result(
    session: &Session,
    cursor: u64,
    format: WatchFormat,
    stdout: &mut impl Write,
) -> Result<u8, WatchError> {
    let (code, fallback) = match (session.lifecycle, session.outcome) {
        (Lifecycle::Exit, Some(Outcome::Success)) => (0, "Session succeeded"),
        (Lifecycle::Exit, Some(Outcome::Failure)) => (1, "Session failed"),
        (Lifecycle::Exit, Some(Outcome::ActionRequired)) => (3, "Session requires action"),
        (Lifecycle::Error, None) => (2, "Session ended with an error"),
        (Lifecycle::Suspended, None) => (130, "Session suspended"),
        _ => {
            return Err(WatchError::Invalid(
                "inconsistent terminal session state".into(),
            ));
        }
    };
    if session.active_attempt_id.is_some() {
        return Err(WatchError::Invalid(
            "terminal session still has an active attempt".into(),
        ));
    }
    match format {
        WatchFormat::Text => writeln!(stdout, "{}", session.result.as_deref().unwrap_or(fallback)),
        WatchFormat::JsonLines => writeln!(
            stdout,
            "{}",
            json!({"type":"result","session":session,"exit_code":code,"after_sequence":cursor})
        ),
    }
    .and_then(|()| stdout.flush())
    .map_err(WatchError::Output)?;
    Ok(code)
}
