use super::*;
use foundry_contracts::{Lifecycle, context::*};
use std::time::Duration;

fn fixture(mode: &str, size: usize) -> (AdapterSpec, Session) {
    let python = Command::new("python3")
        .args(["-c", "import sys;print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let mut spec = AdapterSpec::new(String::from_utf8(python.stdout).unwrap().trim());
    spec.args = [
        "-u",
        "-c",
        include_str!("process_context_fixture.py"),
        mode,
        &size.to_string(),
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    spec.run_timeout = Duration::from_secs(3);
    let session = Session {
        id: "session".into(),
        prompt: "fixture".into(),
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
fn start(process: &Process, session: &Session) {
    process
        .send(&AdapterCommand::Start {
            version: 1,
            attempt_id: "attempt".into(),
            session: session.clone(),
        })
        .unwrap();
}
fn command(size: usize, attempt: &str) -> ContextCommand {
    ContextCommand::RestorePayload {
        attempt_id: ContextId::new(attempt).unwrap(),
        restore_id: ContextId::new("restore").unwrap(),
        payload: ContextPayload::new((0..size).map(|i| (i % 256) as u8).collect()).unwrap(),
        receipt: ContextReceipt::new("01234567-89ab-4cde-8f01-23456789abcd").unwrap(),
    }
}
fn next(receiver: &Receiver<ContextOutput>) -> ContextOutput {
    receiver.recv_timeout(Duration::from_secs(3)).unwrap()
}

#[test]
fn context_opt_in_preserves_order_and_maximum_private_transfer() {
    let (spec, session) = fixture("receive", MAX_PAYLOAD_BYTES);
    let (mut process, events) = Process::spawn_context(&spec, &session).unwrap();
    process
        .send_context(&command(MAX_PAYLOAD_BYTES, "attempt"))
        .unwrap();
    assert!(matches!(
        next(&events),
        ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Ready { .. }))
    ));
    let ContextOutput::Context(ContextEvent::Candidate { payload, .. }) = next(&events) else {
        panic!("private candidate")
    };
    assert_eq!(payload.as_bytes().len(), MAX_PAYLOAD_BYTES);
    assert!(
        payload
            .as_bytes()
            .iter()
            .enumerate()
            .all(|(i, b)| *b == (i % 256) as u8)
    );
    assert!(
        matches!(next(&events),ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Text{text,..})) if text=="ordinary-after-context")
    );
    process.stop().unwrap();
    assert_eq!(process.workers.remaining(), 0);
}

#[test]
fn legacy_paths_reject_context_without_exposing_payload_to_callbacks() {
    let (spec, session) = fixture("normal", 0);
    let (_cancel, receiver) = std::sync::mpsc::channel();
    let mut events = Vec::new();
    let error = crate::run_adapter(&spec, &session, "attempt", &receiver, |event| {
        events.push(event.clone())
    })
    .err()
    .unwrap();
    assert!(matches!(error, RuntimeError::Protocol(_)));
    let diagnostic = format!(
        "{error:?} {error} {}",
        serde_json::to_string(&events).unwrap()
    );
    assert!(!diagnostic.contains("private-sentinel"));
    assert!(!diagnostic.contains("cHJpdmF0ZS1zZW50aW5lbA=="));
    assert_eq!(events.len(), 1);
}

#[test]
fn malformed_context_frames_report_only_fixed_diagnostics_and_join() {
    for mode in [
        "wronghash",
        "wrongattempt",
        "unknownnested",
        "duplicate",
        "truncated",
        "oversize",
    ] {
        let (spec, session) = fixture(mode, 0);
        let (mut process, events) = Process::spawn_context(&spec, &session).unwrap();
        start(&process, &session);
        assert!(matches!(
            next(&events),
            ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Ready { .. }))
        ));
        let ContextOutput::Ordinary(WireEvent::Error(error)) = next(&events) else {
            panic!("expected error: {mode}")
        };
        assert!(!format!("{error:?} {error}").contains("private-sentinel"));
        if mode == "oversize" {
            assert!(matches!(error, RuntimeError::FrameTooLarge));
        } else {
            assert!(matches!(error, RuntimeError::Protocol(_)));
        }
        process.stop().unwrap();
        assert_eq!(process.workers.remaining(), 0);
    }
}

#[test]
fn outbound_context_requires_opt_in_and_matching_attempt() {
    let (spec, session) = fixture("normal", 0);
    let mut legacy = Process::spawn(&spec, &session).unwrap();
    assert!(matches!(
        legacy.send_context(&command(1, "attempt")),
        Err(RuntimeError::Protocol(_))
    ));
    legacy.stop().unwrap();
    let (mut process, _events) = Process::spawn_context(&spec, &session).unwrap();
    assert!(matches!(
        process.send_context(&command(1, "foreign")),
        Err(RuntimeError::Protocol(_))
    ));
    process.stop().unwrap();
}

#[test]
fn full_private_context_queue_is_stoppable_and_joined() {
    let (spec, session) = fixture("flood", 0);
    let (mut process, _events) = Process::spawn_context(&spec, &session).unwrap();
    start(&process, &session);
    std::thread::sleep(Duration::from_millis(100));
    let began = std::time::Instant::now();
    process.stop().unwrap();
    assert!(began.elapsed() < Duration::from_secs(2));
    assert_eq!(process.workers.remaining(), 0);
}

#[test]
fn private_capture_handshake_roundtrips_before_unchanged_start() {
    let (spec, session) = fixture("capture", 0);
    let (mut process, events) = Process::spawn_context(&spec, &session).unwrap();
    assert!(matches!(
        next(&events),
        ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Ready { .. }))
    ));
    let begin = ContextCommand::CaptureBegin {
        attempt_id: ContextId::new("attempt").unwrap(),
        capture_id: ContextId::new("capture").unwrap(),
        format_digest: ContextDigest::of(b"format"),
        configuration_digest: ContextDigest::of(b"configuration"),
        challenge: ContextReceipt::new("01234567-89ab-4cde-8f01-23456789abcd").unwrap(),
    };
    process.send_context(&begin).unwrap();
    let ContextOutput::Context(ContextEvent::CaptureReady {
        attempt_id,
        capture_id,
        format_digest,
        configuration_digest,
        challenge,
    }) = next(&events)
    else {
        panic!("private capture ready")
    };
    assert_eq!(attempt_id.as_str(), "attempt");
    assert_eq!(capture_id.as_str(), "capture");
    assert!(format_digest == ContextDigest::of(b"format"));
    assert!(configuration_digest == ContextDigest::of(b"configuration"));
    assert_eq!(challenge.as_str(), "01234567-89ab-4cde-8f01-23456789abcd");
    start(&process, &session);
    assert!(
        matches!(next(&events), ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Text { text, .. })) if text == "ordinary-after-handshake")
    );
    process.stop().unwrap();
    assert_eq!(process.workers.remaining(), 0);
}

#[test]
fn legacy_runtime_rejects_unsolicited_capture_ready() {
    let (spec, session) = fixture("capture_legacy", 0);
    let (_sender, cancellation) = std::sync::mpsc::channel();
    let mut ordinary = Vec::new();
    let result = crate::run_adapter(&spec, &session, "attempt", &cancellation, |event| {
        ordinary.push(event.clone())
    });
    assert!(matches!(
        result,
        Err(RuntimeError::Protocol("unsolicited context frame"))
    ));
    assert_eq!(ordinary.len(), 1);
    assert!(matches!(ordinary[0], AdapterEvent::Ready { .. }));
}

#[test]
fn checked_context_cleanup_preserves_fault_behind_full_queue_and_joins() {
    let (spec, session) = fixture("faultfull", 0);
    let (mut process, events) = Process::spawn_context(&spec, &session).unwrap();
    assert!(matches!(
        next(&events),
        ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Ready { .. }))
    ));
    start(&process, &session);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !process.cleanup.as_ref().unwrap().has_fault() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let error = process.stop_context_checked(&events).unwrap_err();
    assert!(matches!(
        error,
        RuntimeError::Protocol("capture transport did not close cleanly")
    ));
    assert!(!format!("{error:?}").contains("private"));
    assert_eq!(process.workers.remaining(), 0);
}

#[test]
fn checked_context_cleanup_accepts_idle_shutdown_but_rejects_unconsumed_output() {
    for consume in [true, false] {
        let (spec, session) = fixture("normal", 0);
        let (mut process, events) = Process::spawn_context(&spec, &session).unwrap();
        start(&process, &session);
        assert!(matches!(
            next(&events),
            ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Ready { .. }))
        ));
        assert!(matches!(
            next(&events),
            ContextOutput::Context(ContextEvent::Candidate { .. })
        ));
        if consume {
            assert!(matches!(
                next(&events),
                ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Text { .. }))
            ));
        } else {
            std::thread::sleep(Duration::from_millis(60));
        }
        assert_eq!(process.stop_context_checked(&events).is_ok(), consume);
        assert_eq!(process.workers.remaining(), 0);
    }
}
