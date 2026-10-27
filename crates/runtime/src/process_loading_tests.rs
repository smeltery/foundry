use super::*;
use foundry_contracts::{
    CancelMode,
    context::{ContextId, ContextReceipt},
};
use std::time::Duration;

fn scope() -> LoadingScope {
    LoadingScope {
        restore_id: ContextId::new("restore").unwrap(),
        load_id: ContextReceipt::new("01234567-89ab-4cde-8f01-23456789abcd").unwrap(),
    }
}

fn spec(lines: &[String]) -> AdapterSpec {
    let python = Command::new("python3")
        .args(["-c", "import sys;print(sys.executable)"])
        .output()
        .unwrap();
    assert!(python.status.success());
    let mut spec = AdapterSpec::new(String::from_utf8(python.stdout).unwrap().trim());
    spec.args = vec![
        "-u".into(),
        "-c".into(),
        "import sys,time;[print(line,flush=True) for line in sys.argv[1:]];time.sleep(30)".into(),
    ];
    spec.args.extend(lines.iter().map(Into::into));
    spec
}

fn ready() -> String {
    r#"{"type":"ready","version":1,"capabilities":["context_restore_v2"]}"#.into()
}

fn prepared(scope: LoadingScope) -> LoadingFrame {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type":"restore_prepared_v2", "scope":scope,
        "begin_binding":"00".repeat(32)
    }))
    .unwrap();
    context_v2::decode_frame(&bytes).unwrap()
}

fn next(events: &Receiver<ContextOutput>) -> ContextOutput {
    events.recv_timeout(Duration::from_secs(3)).unwrap()
}

#[test]
fn loading_routes_scoped_response_without_an_attempt_and_joins_workers() {
    let frame = prepared(scope());
    let line = String::from_utf8(context_v2::encode_frame(&frame).unwrap()).unwrap();
    let directory = std::env::temp_dir();
    let (mut process, events) = Process::spawn_loading(
        &spec(&[ready(), line]),
        LoadingProcessSpec {
            directory: &directory,
            scope: scope(),
        },
    )
    .unwrap();
    assert!(process.context_attempt.is_none());
    assert!(matches!(
        next(&events),
        ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Ready { .. }))
    ));
    let ContextOutput::Loading(received) = next(&events) else {
        panic!("loading response expected")
    };
    assert_eq!(received.kind(), FrameKind::Prepared);
    assert!(received.check_scope(&scope()).is_ok());
    assert!(process.send_loading(&frame).is_err());
    assert!(
        process
            .send(&AdapterCommand::Cancel {
                attempt_id: "fake".into(),
                mode: CancelMode::Hard
            })
            .is_err()
    );
    process.stop().unwrap();
    assert_eq!(process.workers.remaining(), 0);
}

#[test]
fn loading_rejects_ordinary_duplicate_early_and_wrong_scope_frames() {
    let mut other = scope();
    other.restore_id = ContextId::new("other").unwrap();
    let encoded =
        |scope| String::from_utf8(context_v2::encode_frame(&prepared(scope)).unwrap()).unwrap();
    for lines in [
        vec![ready(), ready()],
        vec![ready(), r#"{"type":"text","text":"not admitted"}"#.into()],
        vec![ready(), encoded(other)],
        vec![encoded(scope())],
        vec![
            ready(),
            r#"{"type":"context_ready","attempt_id":"fake"}"#.into(),
        ],
    ] {
        let directory = std::env::temp_dir();
        let (mut process, events) = Process::spawn_loading(
            &spec(&lines),
            LoadingProcessSpec {
                directory: &directory,
                scope: scope(),
            },
        )
        .unwrap();
        if lines.len() == 2 {
            assert!(matches!(
                next(&events),
                ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Ready { .. }))
            ));
        }
        assert!(matches!(
            next(&events),
            ContextOutput::Ordinary(WireEvent::Error(_))
        ));
        process.stop().unwrap();
        assert_eq!(process.workers.remaining(), 0);
    }
}

#[test]
fn loading_rejects_relative_directory_before_spawn() {
    assert!(matches!(
        Process::spawn_loading(
            &AdapterSpec::new("/does/not/exist"),
            LoadingProcessSpec {
                directory: Path::new("relative"),
                scope: scope()
            }
        ),
        Err(RuntimeError::Invalid(_))
    ));
}

#[test]
fn loading_requires_restoration_capability_and_compatible_version() {
    for line in [
        r#"{"type":"ready","version":1,"capabilities":[]}"#,
        r#"{"type":"ready","version":2,"capabilities":["context_restore_v2"]}"#,
    ] {
        let directory = std::env::temp_dir();
        let (mut process, events) = Process::spawn_loading(
            &spec(&[line.into()]),
            LoadingProcessSpec {
                directory: &directory,
                scope: scope(),
            },
        )
        .unwrap();
        assert!(matches!(
            next(&events),
            ContextOutput::Ordinary(WireEvent::Error(_))
        ));
        assert!(process.stop_context_checked(&events).is_err());
        assert_eq!(process.workers.remaining(), 0);
    }
}

#[test]
fn loading_fault_behind_full_queue_survives_stop_and_reap() {
    let line = String::from_utf8(context_v2::encode_frame(&prepared(scope())).unwrap()).unwrap();
    let directory = std::env::temp_dir();
    let (mut process, events) = Process::spawn_loading(
        &spec(&[ready(), line, "not-json".into()]),
        LoadingProcessSpec {
            directory: &directory,
            scope: scope(),
        },
    )
    .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !process.cleanup.as_ref().unwrap().has_fault() {
        assert!(
            std::time::Instant::now() < deadline,
            "reader did not observe fault"
        );
        thread::sleep(Duration::from_millis(2));
    }
    let started = std::time::Instant::now();
    assert!(process.stop_context_checked(&events).is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(process.workers.remaining(), 0);
    assert!(process.child.try_wait().unwrap().is_some());
}

#[test]
fn loading_blocked_input_is_bounded_and_stoppable() {
    use foundry_contracts::context::{ContextDigest, ContextPayload, MAX_PAYLOAD_BYTES};
    let directory = std::env::temp_dir();
    // The child announces readiness but never reads stdin.
    let (mut process, events) = Process::spawn_loading(
        &spec(&[ready()]),
        LoadingProcessSpec {
            directory: &directory,
            scope: scope(),
        },
    )
    .unwrap();
    assert!(matches!(
        next(&events),
        ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Ready { .. }))
    ));
    let payload = ContextPayload::new(vec![42; MAX_PAYLOAD_BYTES]).unwrap();
    let frame = LoadingFrame::payload(
        scope(),
        ContextDigest::of(b"begin"),
        &payload,
        scope().load_id,
    )
    .unwrap();
    let mut submitted = 0;
    loop {
        match process.send_loading(&frame) {
            Ok(()) => {
                submitted += 1;
                assert!(submitted <= 3, "blocked writer exceeded bounded queue");
            }
            Err(RuntimeError::CommandQueue) => break,
            Err(error) => panic!("unexpected submission error: {error}"),
        }
    }
    assert!(submitted > 0);
    let started = std::time::Instant::now();
    process.stop().unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(process.workers.remaining(), 0);
    assert!(process.child.try_wait().unwrap().is_some());
}

#[test]
fn loading_uses_explicit_directory_and_shared_environment_allowlist() {
    let mut adapter = spec(&[]);
    adapter.args = vec![
        "-u".into(),
        "-c".into(),
        r#"
import os,sys,time
assert os.getcwd()==sys.argv[1]
assert set(os.environ)<=set(['PATH','LANG','LC_ALL','TZ','LC_CTYPE','__CF_USER_TEXT_ENCODING'])
assert 'HOME' not in os.environ
print('{"type":"ready","version":1,"capabilities":["context_restore_v2"]}',flush=True)
time.sleep(30)
"#
        .into(),
    ];
    // Python/macOS may add locale metadata after exec; these are not passed credentials.
    let directory = std::env::temp_dir().canonicalize().unwrap();
    adapter.args.push(directory.as_os_str().to_owned());
    let (mut process, events) = Process::spawn_loading(
        &adapter,
        LoadingProcessSpec {
            directory: &directory,
            scope: scope(),
        },
    )
    .unwrap();
    assert!(matches!(
        next(&events),
        ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Ready { .. }))
    ));
    process.stop_context_checked(&events).unwrap();
    assert_eq!(process.workers.remaining(), 0);
}

#[test]
fn loading_sends_payload_but_cannot_continue_or_cross_scopes() {
    use context_v2::ContinuationIdentity;
    use foundry_contracts::context::{ContextDigest, ContextPayload};
    let mut adapter = spec(&[]);
    adapter.args = vec![
        "-u".into(),
        "-c".into(),
        r#"
import json,sys,time
print('{"type":"ready","version":1,"capabilities":["context_restore_v2"]}',flush=True)
frame=json.loads(sys.stdin.readline())
assert frame['type']=='restore_payload_v2'
print(json.dumps({'type':'context_restored_v2','scope':frame['scope'],
 'begin_binding':frame['begin_binding'],'payload_digest':frame['payload_digest'],
 'receipt':frame['receipt']}),flush=True)
time.sleep(30)
"#
        .into(),
    ];
    let directory = std::env::temp_dir();
    let (mut process, events) = Process::spawn_loading(
        &adapter,
        LoadingProcessSpec {
            directory: &directory,
            scope: scope(),
        },
    )
    .unwrap();
    assert!(matches!(
        next(&events),
        ContextOutput::Ordinary(WireEvent::Event(AdapterEvent::Ready { .. }))
    ));
    let receipt = scope().load_id;
    let continuation = LoadingFrame::continuation(
        scope(),
        ContinuationIdentity {
            attempt_id: ContextId::new("attempt").unwrap(),
            continuation_id: ContextId::new("continue").unwrap(),
            input_id: ContextId::new("input").unwrap(),
            input_sequence: 1,
            input_digest: ContextDigest::of(b"hello"),
            input_bytes: 5,
            authorization_binding: ContextDigest::of(b"authority"),
        },
        "hello",
        receipt.clone(),
    )
    .unwrap();
    assert!(process.send_loading(&continuation).is_err());
    let payload = ContextPayload::new(vec![0, 255, 10]).unwrap();
    let binding = ContextDigest::of(b"begin");
    let mut wrong = scope();
    wrong.restore_id = ContextId::new("other").unwrap();
    assert!(
        process
            .send_loading(
                &LoadingFrame::payload(wrong, binding, &payload, receipt.clone()).unwrap()
            )
            .is_err()
    );
    process
        .send_loading(&LoadingFrame::payload(scope(), binding, &payload, receipt.clone()).unwrap())
        .unwrap();
    let ContextOutput::Loading(response) = next(&events) else {
        panic!("restored response expected")
    };
    assert_eq!(response.kind(), FrameKind::Restored);
    assert!(response.check_scope(&scope()).is_ok());
    process.stop().unwrap();
    assert_eq!(process.workers.remaining(), 0);
}
