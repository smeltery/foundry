use super::{support::Harness, *};
use crate::coordinated::{
    CaptureExit, FreshCaptureOptions, RuntimePersistence, capture_owner::CaptureOwner,
};
use foundry_contracts::{Lifecycle, Session};
use std::{sync::mpsc, thread};

fn activate(h: &mut Harness) {
    let (_, digest) = h.grant();
    let job = h.accepted(digest);
    h.replies
        .send(ContinuationReply::Activated { job })
        .ok()
        .unwrap();
    assert!(h.pending.poll(&h.cancellation).unwrap());
}
fn session() -> Session {
    Session {
        id: "session".into(),
        prompt: "historical".into(),
        directory: std::env::temp_dir().to_string_lossy().into(),
        model: None,
        lifecycle: Lifecycle::Running,
        outcome: None,
        result: None,
        active_attempt_id: Some("new".into()),
        version: 2,
    }
}
#[test]
fn pending_cancel_before_activation_is_consuming_and_bound_to_same_cleanup() {
    let mut h = Harness::new("continue_ok");
    let (_, digest) = h.grant();
    let _job = h.accepted(digest);
    let observer = h.pending.cleanup_observer();
    let pid = h.pending.process_id();
    h.cancel.send(CancelMode::Graceful).unwrap();
    let error = h.pending.poll(&h.cancellation).unwrap_err();
    assert!(matches!(error.cause, LoadingCause::Cancelled));
    assert!(error.cleanup.is_none());
    let marker = h.pending.close_cancelled(&h.cancellation).unwrap();
    assert!(marker.matches_cleanup(&observer));
    assert_eq!(marker.attempt_id().unwrap().as_str(), "new");
    assert_eq!(marker.mode(), CancelMode::Graceful);
    assert_eq!(observer.status(), CleanupStatus::Complete);
    assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
    let other = Harness::new("continue_quiet");
    assert!(!marker.matches_cleanup(&other.pending.cleanup_observer()));
    other.pending.close().unwrap();
    assert!(
        h.fixture
            .records()
            .iter()
            .all(|r| r["kind"] != "start" && r["kind"] != "tool_result")
    );
}
#[test]
fn admitted_cancel_before_capture_ports_closes_actual_process() {
    let mut h = Harness::new("continue_ok");
    activate(&mut h);
    let observer = h.pending.cleanup_observer();
    let admitted = h.pending.into_admitted().unwrap();
    h.cancel.send(CancelMode::Hard).unwrap();
    let marker = admitted.close_cancelled(&h.cancellation).unwrap();
    assert!(marker.matches_cleanup(&observer));
    assert_eq!(marker.mode(), CancelMode::Hard);
    assert_eq!(marker.attempt_id().unwrap().as_str(), "new");
}
#[test]
fn capture_handoff_cancel_is_existing_owner_bound_proof_and_checks_selection() {
    for mode in ["cancel", "invalid", "disconnect"] {
        let mut h = Harness::new("continue_capture");
        activate(&mut h);
        let observer = h.pending.cleanup_observer();
        let admitted = h.pending.into_admitted().unwrap();
        if mode == "disconnect" {
            drop(h.cancel);
        } else {
            h.cancel.send(CancelMode::Graceful).unwrap();
        }
        let (owner, endpoint) = CaptureOwner::channel();
        let (_sent, messages) = mpsc::channel();
        let (requests, received) = mpsc::sync_channel(2);
        let (_reply, replies) = mpsc::sync_channel(2);
        let mut session = session();
        if mode == "invalid" {
            session.active_attempt_id = Some("other".into());
        }
        let result = admitted.run_restored_capture(
            &h.fixture.spec,
            &session,
            &h.cancellation,
            &messages,
            RuntimePersistence { requests, replies },
            FreshCaptureOptions {
                profile: super::capture_tests::profile(),
                owner,
                policy: None,
                phase_timeout: Duration::from_secs(2),
            },
        );
        if mode != "cancel" {
            assert!(result.is_err());
        } else {
            let CaptureExit::Cancelled(stop) = result.unwrap() else {
                panic!()
            };
            assert!(endpoint.matches_cancelled(&stop));
            assert_eq!(stop.attempt_id(), "new");
            assert_eq!(stop.session_id(), "session");
            assert_eq!(observer.status(), CleanupStatus::Complete);
        }
        assert!(received.try_recv().is_err());
        assert!(endpoint.requests.try_recv().is_err());
    }
}
#[test]
fn disconnect_and_prior_protocol_failure_cannot_be_relabelled_as_cancellation() {
    let mut h = Harness::new("continue_quiet");
    activate(&mut h);
    let admitted = h.pending.into_admitted().unwrap();
    drop(h.cancel);
    let error = match admitted.close_cancelled(&h.cancellation) {
        Err(e) => e,
        Ok(_) => panic!(),
    };
    assert!(matches!(error.cause, LoadingCause::OwnerDisconnected));

    let mut h = Harness::new("continue_identity");
    h.grant();
    let end = Instant::now() + Duration::from_secs(2);
    loop {
        if let Err(error) = h.pending.poll(&h.cancellation) {
            assert!(!matches!(error.cause, LoadingCause::Cancelled));
            break;
        }
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(2));
    }
    let _ = h.cancel.send(CancelMode::Hard);
    assert!(h.pending.close_cancelled(&h.cancellation).is_err());
}
