use super::*;
use foundry_contracts::{CancelMode, Lifecycle};
use foundry_runtime::coordinated::loading::CleanupStatus;
use foundry_service::restoration_owner::{ContinueSelection, RestoreReply};

#[test]
fn post_s2_pending_admitted_and_capture_handoffs_settle_exact_cancellation() {
    let mut prior_transition = None;
    for boundary in ["pending", "admitted", "capture"] {
        let mut h = restoration_support::Harness::new(
            0,
            if boundary == "capture" {
                "continue_capture"
            } else {
                "continue_quiet"
            },
        );
        h.validated();
        let mut store = Store::open(&h.fixture.path).unwrap();
        let head = store.checkpoint_head(&h.fixture.session).unwrap().unwrap();
        let message = store
            .enqueue_message(&h.fixture.session, "next", "continue")
            .unwrap();
        h.owner
            .open_continuation(ContinueSelection {
                request_id: "stop-next".into(),
                message_id: message.id.clone(),
                sequence: message.sequence,
                text_digest: ContextDigest::of(message.text.as_bytes()),
            })
            .unwrap();
        let RestoreReply::Continuation(endpoint) =
            restoration_support::receive(&mut h.owner).result.unwrap()
        else {
            panic!()
        };
        let mut pending = h
            .loaded
            .continue_with(h.binding, endpoint, &h.cancelled)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        if boundary == "pending" {
            while store
                .messages(&h.fixture.session, message.sequence - 1, 1)
                .unwrap()[0]
                .state
                == foundry_contracts::MessageState::Pending
            {
                assert!(!pending.poll(&h.cancelled).unwrap());
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(2));
            }
            let lock = rusqlite::Connection::open(&h.fixture.path).unwrap();
            lock.execute_batch("BEGIN IMMEDIATE").unwrap();
            for _ in 0..30 {
                assert!(!pending.poll(&h.cancelled).unwrap(), "S2 commit is held");
                thread::sleep(Duration::from_millis(2));
            }
            lock.execute_batch("ROLLBACK").unwrap();
            // No runtime polling after SQL release: Activated cannot be consumed.
            while store
                .messages(&h.fixture.session, message.sequence - 1, 1)
                .unwrap()[0]
                .state
                != foundry_contracts::MessageState::Delivered
            {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(2));
            }
        } else {
            while !pending.poll(&h.cancelled).unwrap() {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(2));
            }
        }
        let observer = pending.cleanup_observer();
        let mut transition = None;
        let mut capture = None;
        let mut foreign_capture = None;
        if boundary == "pending" {
            h.owner.cancel(CancelMode::Graceful);
            assert!(pending.poll(&h.cancelled).is_err());
            let marker = pending.close_cancelled(&h.cancelled).unwrap();
            assert!(
                marker.attempt_id().is_some(),
                "Granted identity precedes accepted S2"
            );
            transition = Some(marker);
        } else {
            let admitted = pending.into_admitted().unwrap();
            if boundary == "admitted" {
                h.owner.cancel(CancelMode::Graceful);
                transition = Some(admitted.close_cancelled(&h.cancelled).unwrap());
            } else {
                let mut configuration = configuration();
                configuration.profile.configuration_digest =
                    raw(ContextDigest::of(b"effective-fixture-policy"));
                h.owner.open_capture(configuration).unwrap();
                let RestoreReply::CaptureReady(ports) =
                    restoration_support::receive(&mut h.owner).result.unwrap()
                else {
                    panic!()
                };
                foreign_capture = Some(foreign_capture_stop(&ports.session, &ports.attempt_id));
                h.owner.cancel(CancelMode::Graceful);
                let result = admitted
                    .run_restored_capture(
                        &AdapterSpec::new("/not-spawned"),
                        &ports.session,
                        &h.cancelled,
                        &ports.messages,
                        ports.persistence,
                        FreshCaptureOptions {
                            profile: ports.profile,
                            owner: ports.owner,
                            policy: None,
                            phase_timeout: Duration::from_secs(2),
                        },
                    )
                    .unwrap();
                let CaptureExit::Cancelled(stop) = result else {
                    panic!()
                };
                capture = Some(stop);
            }
        }
        assert_eq!(observer.status(), CleanupStatus::Complete);
        let mut report = h.owner.shutdown();
        assert!(
            report.operation.is_err(),
            "original cancellation operation must remain intact"
        );
        assert!(report.release.is_ok());
        assert!(!report.expired && !report.watchdog_panicked);
        let settlement = report
            .cancellation
            .take()
            .expect("checked post-S2 settlement");
        assert_eq!(settlement.message_id(), message.id);
        if let Some(prior) = &prior_transition {
            assert!(
                !settlement.matches_transition(prior),
                "foreign scope and cleanup cannot match"
            );
        }
        if let Some(marker) = transition {
            assert!(settlement.matches_transition(&marker));
            prior_transition = Some(marker);
        }
        if let Some(marker) = capture {
            assert!(settlement.matches_capture(&marker));
            let foreign = foreign_capture.unwrap();
            assert_eq!(foreign.session_id(), marker.session_id());
            assert_eq!(foreign.attempt_id(), marker.attempt_id());
            assert!(!settlement.matches_capture(&foreign));
        }
        let done = settlement.into_session();
        assert_eq!(done.lifecycle, Lifecycle::Suspended);
        assert!(done.active_attempt_id.is_none());
        assert_eq!(store.get(&done.id).unwrap(), done);
        assert_eq!(store.checkpoint_head(&done.id).unwrap().unwrap(), head);
    }
}

#[test]
fn cancellation_before_s2_cannot_mint_post_s2_settlement() {
    let mut h = restoration_support::Harness::new(0, "continue_quiet");
    h.validated();
    h.owner.cancel(CancelMode::Graceful);
    h.loaded.close().unwrap();
    let report = h.owner.shutdown();
    assert!(report.cancellation.is_none());
}

fn foreign_capture_stop(
    session: &foundry_contracts::Session,
    attempt: &str,
) -> Box<foundry_runtime::coordinated::CaptureCancelled> {
    use foundry_runtime::coordinated::{RuntimePersistence, capture_owner::CaptureOwner};
    let (owner, _endpoint) = CaptureOwner::channel();
    let (cancel, cancelled) = mpsc::channel();
    let (_sender, messages) = mpsc::channel();
    let (requests, _events) = mpsc::sync_channel(1);
    let (_replies, replies) = mpsc::sync_channel(1);
    cancel.send(CancelMode::Graceful).unwrap();
    let result = run_fresh_capture(
        &AdapterSpec::new("/not-spawned"),
        session,
        attempt,
        &cancelled,
        &messages,
        RuntimePersistence { requests, replies },
        FreshCaptureOptions {
            profile: configuration().profile,
            owner,
            policy: None,
            phase_timeout: Duration::from_secs(2),
        },
    )
    .unwrap();
    let CaptureExit::Cancelled(stop) = result else {
        panic!()
    };
    stop
}
