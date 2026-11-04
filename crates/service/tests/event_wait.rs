use foundry_contracts::{ContractError, CreateSession, Lifecycle, Outcome};
use foundry_service::{Service, ServiceError};
use foundry_store::{AttemptFence, DeliveryCursor, EventRead, Store, StoreError};
use serde_json::json;
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

const CHILD: &str = "FOUNDRY_EVENT_WAIT_CHILD";

fn running(path: &Path) -> (Service, Store, AttemptFence) {
    let mut service = Service::open(path).unwrap();
    let session = service
        .create(CreateSession {
            prompt: "wait".into(),
            directory: "/workspace/sample".into(),
            model: None,
            idempotency_key: None,
            ..Default::default()
        })
        .unwrap();
    let mut writer = Store::open(path).unwrap();
    let fence = writer.begin_attempt(&session.id, 0).unwrap();
    (service, writer, fence)
}

fn waiting(after_seq: DeliveryCursor, wait_ms: u32) -> EventRead {
    EventRead {
        after_seq,
        wait_ms: Some(wait_ms),
        ..Default::default()
    }
}

/// Child half of the cross-process test: commits one event after a delay.
#[test]
fn child_commits_an_event_when_spawned() {
    let Ok(value) = std::env::var(CHILD) else {
        return;
    };
    let fields: Vec<&str> = value.split('\n').collect();
    let [path, session_id, attempt_id, token] = fields[..] else {
        panic!("child arguments are incomplete");
    };
    let mut store = Store::open(path).unwrap();
    let fence = AttemptFence {
        session_id: session_id.into(),
        attempt_id: attempt_id.into(),
        token: token.into(),
    };
    std::thread::sleep(Duration::from_millis(300));
    store
        .append_event(&fence, "child", 0, "text", json!("from child"))
        .unwrap();
}

#[test]
fn commit_from_another_process_wakes_a_long_poll() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let (mut service, _writer, fence) = running(&path);
    let payload = [
        path.to_str().unwrap(),
        &fence.session_id,
        &fence.attempt_id,
        &fence.token,
    ]
    .join("\n");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_commits_an_event_when_spawned",
            "--nocapture",
        ])
        .env(CHILD, payload)
        .spawn()
        .unwrap();
    let started = Instant::now();
    let page = service
        .read_events(&fence.session_id, &waiting(DeliveryCursor::Start, 20_000))
        .unwrap();
    let elapsed = started.elapsed();
    assert!(child.wait().unwrap().success());
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].event.id, "child");
    assert_eq!(page.next_seq, DeliveryCursor::After(1));
    assert!(elapsed >= Duration::from_millis(250), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(15), "{elapsed:?}");
}

#[test]
fn empty_long_poll_returns_cleanly_when_the_budget_ends() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let (mut service, mut writer, fence) = running(&path);
    writer
        .append_event(&fence, "seen", 0, "text", json!(1))
        .unwrap();
    let started = Instant::now();
    let page = service
        .read_events(&fence.session_id, &waiting(DeliveryCursor::After(1), 300))
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(page.events.is_empty());
    assert_eq!(page.next_seq, DeliveryCursor::After(1));
    assert_eq!(page.session.lifecycle, Lifecycle::Running);

    // A zero budget answers at once; a settled session never waits.
    let started = Instant::now();
    let read = waiting(DeliveryCursor::After(1), 0);
    assert!(
        service
            .read_events(&fence.session_id, &read)
            .unwrap()
            .events
            .is_empty()
    );
    writer
        .finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Success), None)
        .unwrap();
    let read = waiting(DeliveryCursor::After(1), 20_000);
    let page = service.read_events(&fence.session_id, &read).unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(page.session.lifecycle, Lifecycle::Exit);
}

#[test]
fn out_of_range_wait_and_page_are_invalid_before_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let (mut service, _writer, fence) = running(&dir.path().join("db"));
    let requests = [
        waiting(DeliveryCursor::Start, 20_001),
        EventRead {
            limit: Some(0),
            ..Default::default()
        },
        EventRead {
            limit: Some(1001),
            wait_ms: Some(20_000),
            ..Default::default()
        },
    ];
    for request in requests {
        let started = Instant::now();
        let error = service
            .read_events(&fence.session_id, &request)
            .unwrap_err();
        assert!(matches!(
            error,
            ServiceError::Store(StoreError::Contract(ContractError::Invalid(_)))
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
