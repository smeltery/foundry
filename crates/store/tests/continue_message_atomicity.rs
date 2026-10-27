use support_restore::checkpoint as checkpoint_support;
mod support_restore;
use foundry_contracts::{Lifecycle, MessageState, Outcome};
use foundry_store::{
    ContinueMessageDecision, ContinueMessageInput, RestorePhase, Store, TerminalProposal,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use support_restore::*;

fn input() -> ContinueMessageInput {
    ContinueMessageInput {
        authorization: continuation("continue"),
        message_id: "message".into(),
        expected_text_digest: Sha256::digest(b"next").into(),
    }
}

#[test]
fn every_write_stage_rolls_back_and_retry_can_issue_first_permit() {
    for trigger in [
        "CREATE TRIGGER fail BEFORE INSERT ON attempts BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail BEFORE UPDATE ON sessions BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail BEFORE UPDATE ON messages BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail BEFORE UPDATE ON messages BEGIN SELECT RAISE(IGNORE); END",
        "CREATE TRIGGER fail BEFORE INSERT ON context_continue_messages BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail BEFORE UPDATE ON context_restores WHEN NEW.phase='continuing' BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail AFTER INSERT ON context_continue_messages BEGIN UPDATE context_restores SET expires_ms=1; END",
        "CREATE TRIGGER fail AFTER INSERT ON context_continue_messages BEGIN UPDATE attempts SET cancel='hard' WHERE active=1; END",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        let mut store = Store::open(&path).unwrap();
        let (session, checkpoint) = capture(&mut store);
        let owner = begin(&mut store, &session, &checkpoint, "restore");
        store.confirm_restored(&owner, [7; 32]).unwrap();
        store.enqueue_message(&session, "message", "next").unwrap();
        let before = store.get(&session).unwrap();
        let db = Connection::open(&path).unwrap();
        db.execute_batch(trigger).unwrap();
        assert!(
            store.authorize_continue_message(&owner, input()).is_err(),
            "{trigger}"
        );
        assert_eq!(store.get(&session).unwrap(), before);
        assert_eq!(
            store.work_revision(&session).unwrap(),
            checkpoint.cut.work_revision
        );
        assert_eq!(
            store.messages(&session, 0, 1).unwrap()[0].state,
            MessageState::Pending
        );
        assert_eq!(
            store
                .restore_request(&session, "restore")
                .unwrap()
                .unwrap()
                .phase,
            RestorePhase::Restored
        );
        let counts: (i64, i64) = db.query_row("SELECT (SELECT COUNT(*) FROM context_continue_messages),(SELECT COUNT(*) FROM attempts WHERE active=1)", [], |r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(counts, (0, 0));
        db.execute_batch("DROP TRIGGER fail").unwrap();
        assert!(matches!(
            store.authorize_continue_message(&owner, input()).unwrap(),
            ContinueMessageDecision::Granted { .. }
        ));
    }
}

#[test]
fn second_revision_overflow_rolls_back_admission_claim_and_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let session = checkpoint_support::session(&mut store);
    let attempt = store.begin_attempt(&session, 0).unwrap();
    let db = Connection::open(path).unwrap();
    db.execute("UPDATE session_work SET revision=?1", [i64::MAX - 1])
        .unwrap();
    let checkpoint = store
        .finish_with_opaque_checkpoint(
            &attempt,
            checkpoint_support::input(&store, &attempt, "capture"),
            TerminalProposal {
                lifecycle: Lifecycle::Exit,
                outcome: Some(Outcome::Success),
                result: None,
            },
        )
        .unwrap()
        .checkpoint;
    let owner = begin(&mut store, &session, &checkpoint, "restore");
    store.confirm_restored(&owner, [7; 32]).unwrap();
    store.enqueue_message(&session, "message", "next").unwrap();
    let before = store.get(&session).unwrap();
    assert!(store.authorize_continue_message(&owner, input()).is_err());
    assert_eq!(store.get(&session).unwrap(), before);
    assert_eq!(
        store.work_revision(&session).unwrap(),
        (i64::MAX - 1) as u64
    );
    assert_eq!(
        store.messages(&session, 0, 1).unwrap()[0].state,
        MessageState::Pending
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM attempts", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM context_continue_messages", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
}

#[test]
fn concurrent_same_and_different_requests_issue_only_one_permit() {
    for different in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        let mut store = Store::open(&path).unwrap();
        let (session, checkpoint) = capture(&mut store);
        let owner = std::sync::Arc::new(begin(&mut store, &session, &checkpoint, "restore"));
        store.confirm_restored(&owner, [7; 32]).unwrap();
        store.enqueue_message(&session, "message", "next").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let workers: Vec<_> = (0..2)
            .map(|index| {
                let path = path.clone();
                let owner = owner.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut store = Store::open(path).unwrap();
                    let mut request = input();
                    if different {
                        request.authorization.request_id = format!("request-{index}");
                    }
                    barrier.wait();
                    match store.authorize_continue_message(&owner, request) {
                        Ok(ContinueMessageDecision::Granted { .. }) => 1,
                        Ok(ContinueMessageDecision::AlreadyAuthorized(_)) => 2,
                        Err(_) => 3,
                    }
                })
            })
            .collect();
        let mut results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        results.sort();
        assert_eq!(results, if different { vec![1, 3] } else { vec![1, 2] });
        assert_eq!(
            store.work_revision(&session).unwrap(),
            checkpoint.cut.work_revision + 2
        );
    }
}

#[test]
fn withdrawal_and_cancellation_serialize_with_bound_transfer() {
    for cancel in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        let mut store = Store::open(&path).unwrap();
        let (session, checkpoint) = capture(&mut store);
        let owner = begin(&mut store, &session, &checkpoint, "restore");
        store.confirm_restored(&owner, [7; 32]).unwrap();
        store.enqueue_message(&session, "message", "next").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let other_barrier = barrier.clone();
        let mut contender = Store::open(&path).unwrap();
        let other_session = session.clone();
        let other = std::thread::spawn(move || {
            other_barrier.wait();
            if cancel {
                contender
                    .request_cancel(&other_session, foundry_contracts::CancelMode::Hard)
                    .map(|_| ())
            } else {
                contender
                    .withdraw_message(&other_session, "message")
                    .map(|_| ())
            }
        });
        barrier.wait();
        let result = store.authorize_continue_message(&owner, input());
        let competing = other.join().unwrap();
        let message = store.messages(&session, 0, 1).unwrap().remove(0);
        if let Ok(ContinueMessageDecision::Granted { permit, .. }) = result {
            assert_eq!(message.state, MessageState::InFlight);
            assert_eq!(
                store.work_revision(&session).unwrap(),
                checkpoint.cut.work_revision + 2
            );
            let (attempt, _) = permit.into_delivery();
            if cancel {
                competing.unwrap();
                assert_eq!(
                    store.cancellation(&attempt).unwrap(),
                    Some(foundry_contracts::CancelMode::Hard)
                );
            } else {
                assert!(competing.is_err());
            }
        } else {
            assert!(result.is_err());
            competing.unwrap();
            assert_eq!(
                message.state,
                if cancel {
                    MessageState::Pending
                } else {
                    MessageState::Withdrawn
                }
            );
            assert_eq!(
                store.work_revision(&session).unwrap(),
                checkpoint.cut.work_revision
            );
            assert!(store.get(&session).unwrap().active_attempt_id.is_none());
        }
    }
}
