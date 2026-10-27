#[path = "../src/watch.rs"]
mod watch;

use foundry_contracts::{CancelMode, CreateSession, Lifecycle, Outcome, Session};
use foundry_store::Store;
use serde_json::json;
use std::{
    io::{self, Write},
    path::PathBuf,
    time::Duration,
};
use tempfile::{TempDir, tempdir};
use watch::{WatchError, WatchFormat, WatchOptions, watch};

struct Fixture {
    _directory: TempDir,
    database: PathBuf,
    session: Session,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempdir().unwrap();
        let database = directory.path().join("sessions.db");
        let session = Store::open(&database)
            .unwrap()
            .create(CreateSession {
                prompt: "offline task".into(),
                directory: directory.path().display().to_string(),
                model: None,
                idempotency_key: None,
            })
            .unwrap();
        Self {
            _directory: directory,
            database,
            session,
        }
    }
}

fn options(format: WatchFormat) -> WatchOptions {
    WatchOptions {
        format,
        interval: Duration::from_millis(2),
        timeout: Some(Duration::from_secs(5)),
        ..WatchOptions::default()
    }
}
fn follow(
    fixture: &Fixture,
    config: WatchOptions,
    stdout: &mut impl Write,
) -> Result<u8, WatchError> {
    watch(
        &fixture.database,
        &fixture.session.id,
        config,
        stdout,
        &mut Vec::new(),
    )
}
#[test]
fn timeout_does_not_cancel_or_claim_completion() {
    let fixture = Fixture::new();
    let mut config = options(WatchFormat::JsonLines);
    config.interval = Duration::from_secs(10);
    config.timeout = Some(Duration::from_millis(20));
    let mut output = Vec::new();
    assert!(matches!(
        follow(&fixture, config, &mut output),
        Err(WatchError::TimedOut)
    ));
    assert!(output.is_empty());
    assert_eq!(
        Store::open(&fixture.database)
            .unwrap()
            .get(&fixture.session.id)
            .unwrap()
            .lifecycle,
        Lifecycle::Queued
    );
    assert!(WatchOptions::default().timeout.is_none());
}

#[test]
fn missing_corrupt_databases_and_missing_sessions_propagate_errors() {
    let fixture = Fixture::new();
    let missing = fixture.database.with_file_name("missing.db");
    let run = |path: &PathBuf, id: &str| {
        watch(
            path,
            id,
            options(WatchFormat::Text),
            &mut Vec::new(),
            &mut Vec::new(),
        )
    };
    assert!(matches!(
        run(&missing, "id"),
        Err(WatchError::DatabasePath(_))
    ));
    assert!(!missing.exists());
    std::fs::write(&missing, b"invalid database").unwrap();
    assert!(matches!(run(&missing, "id"), Err(WatchError::Storage(_))));
    assert!(matches!(
        run(&fixture.database, "missing"),
        Err(WatchError::Storage(_))
    ));
}

struct FailingWriter {
    on_flush: bool,
}
impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.on_flush {
            Ok(bytes.len())
        } else {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
}

#[test]
fn event_and_result_write_or_flush_failures_propagate() {
    for event in [false, true] {
        for format in [WatchFormat::Text, WatchFormat::JsonLines] {
            for on_flush in [false, true] {
                let fixture = Fixture::new();
                let mut store = Store::open(&fixture.database).unwrap();
                let fence = store.begin_attempt(&fixture.session.id, 0).unwrap();
                if event {
                    store
                        .append_event(&fence, "part", 0, "text", json!({"text":"progress"}))
                        .unwrap();
                }
                store
                    .finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Success), None)
                    .unwrap();
                let result = watch(
                    &fixture.database,
                    &fixture.session.id,
                    options(format),
                    &mut FailingWriter { on_flush },
                    &mut FailingWriter { on_flush },
                );
                assert!(matches!(result, Err(WatchError::Output(_))));
            }
        }
    }
}

#[test]
fn queued_cancellation_is_terminal_and_invalid_options_do_not_open_database() {
    let fixture = Fixture::new();
    Store::open(&fixture.database)
        .unwrap()
        .request_cancel(&fixture.session.id, CancelMode::Graceful)
        .unwrap();
    assert_eq!(
        follow(&fixture, options(WatchFormat::Text), &mut Vec::new()).unwrap(),
        130
    );
    let missing = fixture.database.with_file_name("absent.db");
    for (interval, timeout, cursor) in [
        (Duration::ZERO, None, 0),
        (Duration::from_secs(1), Some(Duration::ZERO), 0),
        (Duration::from_secs(1), None, u64::MAX),
    ] {
        let config = WatchOptions {
            interval,
            timeout,
            after_sequence: cursor,
            ..WatchOptions::default()
        };
        assert!(matches!(
            watch(&missing, "id", config, &mut Vec::new(), &mut Vec::new()),
            Err(WatchError::Invalid(_))
        ));
    }
    assert!(!missing.exists());
}
