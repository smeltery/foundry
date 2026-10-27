#![cfg(unix)]
use foundry_contracts::{AdapterEvent, Lifecycle, Session};
use foundry_runtime::{
    AdapterMessage, AdapterSpec, RunResult, RuntimeError, run_adapter_with_messages,
};
use std::{process::Command, sync::mpsc, time::Duration};

fn fixture(mode: &str) -> (AdapterSpec, Session) {
    let python = Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let mut spec = AdapterSpec::new(String::from_utf8(python.stdout).unwrap().trim());
    spec.args = ["-u", "-c", include_str!("message_adapter.py"), mode]
        .into_iter()
        .map(Into::into)
        .collect();
    spec.run_timeout = Duration::from_secs(3);
    let session = Session {
        id: "session".into(),
        prompt: "test".into(),
        directory: std::env::temp_dir().to_string_lossy().into(),
        model: None,
        lifecycle: Lifecycle::Running,
        outcome: None,
        result: None,
        active_attempt_id: Some("attempt".into()),
        version: 1,
    };
    (spec, session)
}

#[test]
fn forwards_literal_message_and_accepts_duplicate_acknowledgments() {
    let (spec, session) = fixture("duplicate");
    let (_cancel, cancellation) = mpsc::channel();
    let (sender, messages) = mpsc::channel();
    sender
        .send(AdapterMessage {
            message_id: "m".into(),
            text: "literal $(false) ; data".into(),
        })
        .unwrap();
    let mut acks = 0;
    let result = run_adapter_with_messages(
        &spec,
        &session,
        "attempt",
        &cancellation,
        &messages,
        |event| {
            if matches!(event, AdapterEvent::MessageAccepted { .. }) {
                acks += 1;
            }
        },
    )
    .unwrap();
    assert_eq!(acks, 2);
    assert!(matches!(result,RunResult::Completed {result,..} if result=="literal $(false) ; data"));
}

#[test]
fn missing_or_legacy_capability_does_not_consume_messages() {
    for mode in ["no_capability", "legacy_capability"] {
        let (spec, session) = fixture(mode);
        let (_cancel, cancellation) = mpsc::channel();
        let (sender, messages) = mpsc::channel();
        sender
            .send(AdapterMessage {
                message_id: "m".into(),
                text: "test".into(),
            })
            .unwrap();
        run_adapter_with_messages(&spec, &session, "attempt", &cancellation, &messages, |_| {})
            .unwrap();
        assert_eq!(messages.try_recv().unwrap().message_id, "m");
    }
}

#[test]
fn rejects_foreign_and_stale_attempt_acknowledgments() {
    for mode in ["foreign", "stale_attempt"] {
        let (spec, session) = fixture(mode);
        let (_cancel, cancellation) = mpsc::channel();
        let (sender, messages) = mpsc::channel();
        sender
            .send(AdapterMessage {
                message_id: "m".into(),
                text: "test".into(),
            })
            .unwrap();
        let mut accepted = false;
        let result = run_adapter_with_messages(
            &spec,
            &session,
            "attempt",
            &cancellation,
            &messages,
            |event| {
                accepted |= matches!(event, AdapterEvent::MessageAccepted { .. });
            },
        );
        assert!(matches!(result, Err(RuntimeError::Protocol(_))));
        assert!(!accepted);
    }
}

#[test]
fn early_ack_cannot_become_valid_when_ready_callback_queues_same_id() {
    let (spec, session) = fixture("unsent");
    let (_cancel, cancellation) = mpsc::channel();
    let (sender, messages) = mpsc::channel();
    let mut accepted = false;
    let result = run_adapter_with_messages(
        &spec,
        &session,
        "attempt",
        &cancellation,
        &messages,
        |event| {
            if matches!(event, AdapterEvent::Ready { .. }) {
                sender
                    .send(AdapterMessage {
                        message_id: "m".into(),
                        text: "test".into(),
                    })
                    .unwrap();
                std::thread::sleep(Duration::from_millis(200));
            }
            accepted |= matches!(event, AdapterEvent::MessageAccepted { .. });
        },
    );
    assert!(matches!(result, Err(RuntimeError::Protocol(_))));
    assert!(!accepted);
}

#[test]
fn large_blocked_write_does_not_validate_a_premature_ack() {
    let (spec, session) = fixture("preack_large");
    let (_cancel, cancellation) = mpsc::channel();
    let (sender, messages) = mpsc::channel();
    sender
        .send(AdapterMessage {
            message_id: "m".into(),
            text: "x".repeat(262144),
        })
        .unwrap();
    let mut accepted = false;
    let result = run_adapter_with_messages(
        &spec,
        &session,
        "attempt",
        &cancellation,
        &messages,
        |event| {
            accepted |= matches!(event, AdapterEvent::MessageAccepted { .. });
        },
    );
    assert!(
        matches!(result, Err(RuntimeError::Protocol(_))),
        "{result:?}"
    );
    assert!(!accepted, "premature acknowledgment reached callback");
}

#[test]
fn immediate_reader_ack_is_accepted_without_writer_scheduling_assumptions() {
    for _ in 0..10 {
        let (spec, session) = fixture("fast");
        let (_cancel, cancellation) = mpsc::channel();
        let (sender, messages) = mpsc::channel();
        sender
            .send(AdapterMessage {
                message_id: "m".into(),
                text: "x".repeat(262144),
            })
            .unwrap();
        let mut accepted = 0;
        let result = run_adapter_with_messages(
            &spec,
            &session,
            "attempt",
            &cancellation,
            &messages,
            |event| {
                if matches!(event, AdapterEvent::MessageAccepted { .. }) {
                    accepted += 1;
                }
            },
        );
        assert!(
            matches!(result, Ok(RunResult::Completed { .. })),
            "{result:?}"
        );
        assert_eq!(accepted, 1);
    }
}
