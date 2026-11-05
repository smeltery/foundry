mod restoration_native_support;
use foundry_contracts::{CancelMode, Lifecycle, MessageState, context::ContextDigest};
use foundry_service::{ServiceError, restoration_owner::*};
use foundry_store::{RestorePhase, Store};
use restoration_native_support::*;
use std::{
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};

#[test]
fn registered_comparison_confirms_current_real_s1_without_admitting_an_attempt() {
    let mut h = Harness::new(0, "continue_quiet");
    let validation = h.validated();
    assert!(validation != h.binding.baseline.binding_digest().unwrap());
    let store = Store::open(&h.fixture.path).unwrap();
    assert_eq!(
        store
            .restore_request(&h.fixture.session, "load")
            .unwrap()
            .unwrap()
            .phase,
        RestorePhase::Restored
    );
    assert!(
        store
            .get(&h.fixture.session)
            .unwrap()
            .active_attempt_id
            .is_none()
    );
    h.loaded.close().unwrap();
    assert_eq!(
        h.owner.shutdown().release.unwrap().unwrap().phase,
        RestorePhase::Closed
    );
}

#[test]
fn wrong_or_rejected_comparator_result_never_confirms_or_grants() {
    for mode in [1, 2, 3] {
        let mut h = Harness::new(mode, "continue_quiet");
        let deadline = Instant::now() + Duration::from_secs(2);
        while h.loaded.poll(&h.cancelled).is_ok() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        let report = h.owner.shutdown();
        assert!(report.operation.is_err());
        let store = Store::open(&h.fixture.path).unwrap();
        assert!(
            store
                .get(&h.fixture.session)
                .unwrap()
                .active_attempt_id
                .is_none()
        );
        assert_ne!(
            store
                .restore_request(&h.fixture.session, "load")
                .unwrap()
                .unwrap()
                .phase,
            RestorePhase::Restored
        );
    }
}

#[test]
fn real_grant_and_acceptance_use_retained_job_and_receipt_without_second_attempt() {
    let mut h = Harness::new(0, "continue_quiet");
    h.validated();
    let mut store = Store::open(&h.fixture.path).unwrap();
    let message = store
        .enqueue_message(&h.fixture.session, "next", "continue once")
        .unwrap();
    h.owner
        .open_continuation(ContinueSelection {
            request_id: "continue-once".into(),
            message_id: message.id.clone(),
            sequence: message.sequence,
            text_digest: ContextDigest::of(message.text.as_bytes()),
        })
        .unwrap();
    let RestoreReply::Continuation(endpoint) = receive(&mut h.owner).result.unwrap() else {
        panic!()
    };
    let mut pending = h
        .loaded
        .continue_with(h.binding, endpoint, &h.cancelled)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !pending.poll(&h.cancelled).unwrap() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let session = store.get(&h.fixture.session).unwrap();
    assert_eq!(session.lifecycle, Lifecycle::Running);
    assert!(session.active_attempt_id.is_some());
    let accepted = store.messages(&h.fixture.session, 0, 10).unwrap();
    assert_eq!(accepted[0].state, MessageState::Delivered);
    assert_eq!(accepted[0].attempt_id, session.active_attempt_id);
    assert!(
        store
            .events(&h.fixture.session, 0, 10)
            .unwrap()
            .iter()
            .any(|event| event.kind == "continue_accepted")
    );
    pending.close().unwrap();
    let report = h.owner.shutdown();
    assert!(report.release.is_ok());
    assert!(
        store
            .get(&h.fixture.session)
            .unwrap()
            .active_attempt_id
            .is_none()
    );
}

#[test]
fn completed_runtime_cannot_be_confirmed_by_later_comparator_success() {
    let h = Harness::new(GATED_COMPARATOR, "continue_quiet");
    // close() returns only after runtime cleanup completes; only then may comparison succeed.
    h.loaded.close().unwrap();
    h.comparator_release.store(true, Ordering::Release);
    // Before shutdown, a Hard cancel can only come from the owner ending service itself.
    assert_eq!(
        h.cancelled.recv_timeout(Duration::from_secs(10)),
        Ok(CancelMode::Hard)
    );
    let report = h.owner.shutdown();
    assert!(matches!(
        report.operation,
        Err(ServiceError::Coordination(
            "native runtime cleanup already started"
        ))
    ));
    let store = Store::open(&h.fixture.path).unwrap();
    assert_ne!(
        store
            .restore_request(&h.fixture.session, "load")
            .unwrap()
            .unwrap()
            .phase,
        RestorePhase::Restored
    );
    assert!(
        store
            .get(&h.fixture.session)
            .unwrap()
            .active_attempt_id
            .is_none()
    );
}

#[test]
fn lost_granted_reply_retains_attempt_until_checked_cleanup() {
    let mut h = Harness::new(0, "continue_quiet");
    h.validated();
    let mut store = Store::open(&h.fixture.path).unwrap();
    let message = store
        .enqueue_message(&h.fixture.session, "next", "continue once")
        .unwrap();
    h.owner
        .open_continuation(ContinueSelection {
            request_id: "lost-grant".into(),
            message_id: message.id,
            sequence: message.sequence,
            text_digest: ContextDigest::of(message.text.as_bytes()),
        })
        .unwrap();
    let RestoreReply::Continuation(endpoint) = receive(&mut h.owner).result.unwrap() else {
        panic!()
    };
    let mut pending = h
        .loaded
        .continue_with(h.binding, endpoint, &h.cancelled)
        .unwrap();
    assert!(!pending.poll(&h.cancelled).unwrap());
    let deadline = Instant::now() + Duration::from_secs(2);
    while store
        .get(&h.fixture.session)
        .unwrap()
        .active_attempt_id
        .is_none()
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    // Do not consume Granted or send Continue: close/drop the runtime channel owner.
    pending.close().unwrap();
    let report = h.owner.shutdown();
    assert!(report.release.is_ok());
    assert!(
        store
            .get(&h.fixture.session)
            .unwrap()
            .active_attempt_id
            .is_none()
    );
    assert_eq!(
        store.messages(&h.fixture.session, 0, 1).unwrap()[0].state,
        MessageState::InFlight
    );
    let wire = std::fs::read_to_string(h.fixture.directory.path().join("wire.jsonl")).unwrap();
    assert!(!wire.contains("continue_context_v2"));
}
