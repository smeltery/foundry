#![cfg(unix)]

use foundry_contracts::{AdapterEvent, CancelMode, Lifecycle, Outcome, Session};
use foundry_runtime::{AdapterSpec, RunResult, RuntimeError, run_adapter};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn fixture(scenario: &str) -> (AdapterSpec, Session) {
    let output = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .expect("Python 3 is required for offline adapter fixtures");
    assert!(output.status.success());
    let mut spec = AdapterSpec::new(String::from_utf8(output.stdout).unwrap().trim());
    spec.args = ["-u", "-c", include_str!("fake_adapter.py"), scenario]
        .into_iter()
        .map(Into::into)
        .collect();
    spec.ready_timeout = Duration::from_secs(2);
    spec.run_timeout = Duration::from_secs(3);
    spec.cancel_timeout = Duration::from_millis(100);
    let session = Session {
        id: "session".into(),
        prompt: "offline fixture".into(),
        directory: std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into(),
        model: None,
        lifecycle: Lifecycle::Running,
        outcome: None,
        result: None,
        active_attempt_id: Some("attempt".into()),
        version: 1,
    };
    (spec, session)
}

fn run(scenario: &str) -> Result<RunResult, RuntimeError> {
    let (spec, session) = fixture(scenario);
    let (_sender, cancel) = mpsc::channel();
    run_adapter(&spec, &session, "attempt", &cancel, |_| {})
}

#[test]
fn text_settlement_cwd_and_isolated_environment() {
    let (spec, session) = fixture("text");
    let (_sender, cancel) = mpsc::channel();
    let mut text = String::new();
    let result = run_adapter(&spec, &session, "attempt", &cancel, |event| {
        if let AdapterEvent::Text { text: value, .. } = event {
            text = value.clone();
        }
    })
    .unwrap();
    assert_eq!(text, session.directory);
    assert_eq!(
        result,
        RunResult::Completed {
            outcome: Outcome::Success,
            result: "done".into()
        }
    );
}

#[test]
fn denies_every_tool_call() {
    assert!(matches!(run("tool"), Ok(RunResult::Completed { .. })));
}

#[test]
fn rejects_invalid_frames_versions_and_attempts() {
    assert!(matches!(run("version"), Err(RuntimeError::Protocol(_))));
    assert!(matches!(
        run("duplicate_ready"),
        Err(RuntimeError::Protocol(_))
    ));
    assert!(matches!(run("frame"), Err(RuntimeError::FrameTooLarge)));
    let error = run("malformed").unwrap_err();
    assert!(matches!(error, RuntimeError::Protocol(_)));
    assert!(!error.to_string().contains("secret-do-not-print"));
    let (spec, session) = fixture("stale");
    let (_sender, cancel) = mpsc::channel();
    let mut delivered = false;
    let result = run_adapter(&spec, &session, "attempt", &cancel, |event| {
        delivered |= matches!(event, AdapterEvent::Text { .. });
    });
    assert!(matches!(result, Err(RuntimeError::Protocol(_))));
    assert!(!delivered);
}

#[test]
fn reports_dead_adapter_and_redacts_failure_details() {
    let exit = run("exit");
    assert!(matches!(exit, Err(RuntimeError::Exited)), "{exit:?}");
    let error = run("failed").unwrap_err();
    assert!(matches!(error, RuntimeError::AdapterFailed));
    assert!(!error.to_string().contains("secret-do-not-print"));
}

#[test]
fn drains_large_stderr_without_blocking() {
    assert!(matches!(run("stderr"), Ok(RunResult::Completed { .. })));
}

#[test]
fn graceful_cancel_suppresses_adapter_success() {
    let (spec, session) = fixture("cancel");
    let (sender, cancel) = mpsc::channel();
    let mut settled = false;
    let result = run_adapter(&spec, &session, "attempt", &cancel, |event| {
        if matches!(event, AdapterEvent::Text { .. }) {
            sender.send(CancelMode::Graceful).unwrap();
        }
        settled |= matches!(event, AdapterEvent::Settled { .. });
    })
    .unwrap();
    assert_eq!(
        result,
        RunResult::Cancelled {
            mode: CancelMode::Graceful,
            forced: false
        }
    );
    assert!(!settled);
}

#[test]
fn cancellation_timeout_and_hard_upgrade_are_bounded() {
    for upgrade in [false, true] {
        let (spec, session) = fixture("hang");
        let (sender, cancel) = mpsc::channel();
        let start = Instant::now();
        let result = run_adapter(&spec, &session, "attempt", &cancel, |event| {
            if matches!(event, AdapterEvent::Text { .. }) {
                sender.send(CancelMode::Graceful).unwrap();
                if upgrade {
                    sender.send(CancelMode::Hard).unwrap();
                }
            }
        })
        .unwrap();
        let mode = if upgrade {
            CancelMode::Hard
        } else {
            CancelMode::Graceful
        };
        assert_eq!(result, RunResult::Cancelled { mode, forced: true });
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

#[test]
fn enforces_ready_and_execution_deadlines() {
    for (scenario, ready_timeout) in [("ready_timeout", true), ("hang", false)] {
        let (mut spec, session) = fixture(scenario);
        if ready_timeout {
            spec.ready_timeout = Duration::from_millis(100);
        } else {
            spec.run_timeout = Duration::from_millis(150);
        }
        let (_sender, cancel) = mpsc::channel();
        let result = run_adapter(&spec, &session, "attempt", &cancel, |_| {});
        if ready_timeout {
            assert!(matches!(result, Err(RuntimeError::ReadyTimeout)));
        } else {
            assert!(matches!(result, Err(RuntimeError::RunTimeout)));
        }
    }
}

#[test]
fn hard_cancel_stops_owned_descendants() {
    let (spec, mut session) = fixture("child");
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("f5y-process-{}-{suffix}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    session.directory = directory.to_string_lossy().into();
    let (sender, cancel) = mpsc::channel();
    let result = run_adapter(&spec, &session, "attempt", &cancel, |event| {
        if matches!(event, AdapterEvent::Text { .. }) {
            sender.send(CancelMode::Hard).unwrap();
        }
    });
    std::thread::sleep(Duration::from_millis(600));
    let escaped = directory.join("escaped").exists();
    std::fs::remove_dir_all(directory).unwrap();
    assert!(matches!(
        result,
        Ok(RunResult::Cancelled {
            mode: CancelMode::Hard,
            ..
        })
    ));
    assert!(!escaped, "adapter descendant survived cancellation");
}

#[test]
fn blocked_stdin_does_not_block_deadlines() {
    let (mut spec, mut session) = fixture("blocked_input");
    spec.run_timeout = Duration::from_millis(150);
    session.prompt = "x".repeat(256 * 1024);
    let (_sender, cancel) = mpsc::channel();
    let started = Instant::now();
    let result = run_adapter(&spec, &session, "attempt", &cancel, |_| {});
    assert!(matches!(result, Err(RuntimeError::RunTimeout)));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn repeated_quick_exit_preserves_exit_diagnostic() {
    for _ in 0..25 {
        let result = run("exit");
        assert!(matches!(result, Err(RuntimeError::Exited)), "{result:?}");
    }
}

#[test]
fn failure_callback_contains_only_fixed_diagnostics() {
    let (spec, session) = fixture("failed");
    let (_sender, cancel) = mpsc::channel();
    let mut events = Vec::new();
    let result = run_adapter(&spec, &session, "attempt", &cancel, |event| {
        events.push(event.clone())
    });
    assert!(matches!(result, Err(RuntimeError::AdapterFailed)));
    let failures: Vec<_> = events
        .iter()
        .filter(|event| matches!(event, AdapterEvent::Failed { .. }))
        .collect();
    assert_eq!(
        failures,
        vec![&AdapterEvent::Failed {
            attempt_id: "attempt".into(),
            code: "adapter_failed".into(),
            message: "Adapter reported failure; details withheld".into(),
        }]
    );
    let output = format!("{} {result:?}", serde_json::to_string(&events).unwrap());
    assert!(!output.contains("private"));
    assert!(!output.contains("secret-do-not-print"));
}
