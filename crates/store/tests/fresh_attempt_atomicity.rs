mod support_restore;
use foundry_contracts::{CancelMode, CreateSession};
use foundry_store::{ContinueMessageInput, Store, ToolOutcome};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Barrier};
fn create(store: &mut Store) -> String {
    store
        .create(CreateSession {
            prompt: "fresh".into(),
            directory: "/tmp".into(),
            model: None,
            idempotency_key: None,
        })
        .unwrap()
        .id
}

#[test]
fn every_same_session_history_table_independently_blocks_initial_admission() {
    for table in [
        "attempts",
        "events",
        "tool_calls",
        "context_checkpoints",
        "context_checkpoint_heads",
        "context_restores",
        "context_continue_messages",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let target = create(&mut store);
        let (donor, checkpoint) = support_restore::capture(&mut store);
        let owner = support_restore::begin(&mut store, &donor, &checkpoint, "restore");
        store.confirm_restored(&owner, [7; 32]).unwrap();
        store.enqueue_message(&donor, "input", "next").unwrap();
        let decision = store
            .authorize_continue_message(
                &owner,
                ContinueMessageInput {
                    authorization: support_restore::continuation("next"),
                    message_id: "input".into(),
                    expected_text_digest: Sha256::digest(b"next").into(),
                },
            )
            .unwrap();
        let foundry_store::ContinueMessageDecision::Granted { permit, .. } = decision else {
            panic!("first permit")
        };
        let (attempt, _) = permit.into_delivery();
        store
            .append_event(&attempt, "event", 0, "fixture", serde_json::json!({}))
            .unwrap();
        store.claim_tool(&attempt, "tool", "read", [1; 32]).unwrap();
        store
            .finish_tool(&attempt, "tool", ToolOutcome::Failed, None)
            .unwrap();
        let db = Connection::open(path).unwrap();
        // Isolate one authoritative predicate; other history belongs to a different session.
        db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        db.execute(
            &format!("UPDATE {table} SET session_id=?1 WHERE session_id=?2"),
            [&target, &donor],
        )
        .unwrap();
        if table == "attempts" {
            db.execute("UPDATE attempts SET active=0", []).unwrap();
        }
        let before = store.get(&target).unwrap();
        assert!(store.begin_fresh_attempt(&target, 0).is_err(), "{table}");
        assert_eq!(store.get(&target).unwrap(), before);
        assert_eq!(store.work_revision(&target).unwrap(), 0);
    }
}

#[test]
fn claimed_delivered_or_attributed_queue_is_not_pristine() {
    for update in [
        "state='in_flight'",
        "state='delivered'",
        "state='withdrawn',attempt_id='old'",
        "state='pending',attempt_id='old'",
        "state='unknown'",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = create(&mut store);
        store.enqueue_message(&id, "message", "pending").unwrap();
        let db = Connection::open(path).unwrap();
        db.execute_batch(&format!("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON; UPDATE messages SET {update}")).unwrap();
        assert!(store.begin_fresh_attempt(&id, 0).is_err());
        assert_eq!(store.work_revision(&id).unwrap(), 0);
    }
}

#[test]
fn write_failures_late_expiry_and_cancellation_roll_back_all_admission_state() {
    for trigger in [
        "CREATE TRIGGER fail BEFORE UPDATE ON session_work BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail BEFORE INSERT ON attempts BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail BEFORE UPDATE ON sessions BEGIN SELECT RAISE(ABORT,'injected'); END",
        "CREATE TRIGGER fail AFTER UPDATE ON sessions BEGIN UPDATE attempts SET expires_ms=1; END",
        "CREATE TRIGGER fail AFTER UPDATE ON sessions BEGIN UPDATE attempts SET cancel='hard'; END",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = create(&mut store);
        let before = store.get(&id).unwrap();
        let db = Connection::open(path).unwrap();
        db.execute_batch(trigger).unwrap();
        assert!(store.begin_fresh_attempt(&id, 0).is_err(), "{trigger}");
        assert_eq!(store.get(&id).unwrap(), before);
        assert_eq!(store.work_revision(&id).unwrap(), 0);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM attempts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        db.execute_batch("DROP TRIGGER fail").unwrap();
        assert!(store.begin_fresh_attempt(&id, 0).is_ok());
    }
}

#[test]
fn concurrent_fresh_or_ordinary_admission_has_only_one_winner() {
    for ordinary in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = create(&mut store);
        let mut other = Store::open(path).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let other_barrier = barrier.clone();
        let other_id = id.clone();
        let worker = std::thread::spawn(move || {
            other_barrier.wait();
            if ordinary {
                other.begin_attempt(&other_id, 0).is_ok()
            } else {
                other.begin_fresh_attempt(&other_id, 0).is_ok()
            }
        });
        barrier.wait();
        let first = store.begin_fresh_attempt(&id, 0).is_ok();
        assert_ne!(first, worker.join().unwrap());
        assert_eq!(store.work_revision(&id).unwrap(), 1);
    }
}

#[test]
fn cancellation_or_pending_queue_race_never_loses_cancel_or_claims_input() {
    for cancel in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = create(&mut store);
        let mut other = Store::open(path).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let other_barrier = barrier.clone();
        let other_id = id.clone();
        let worker = std::thread::spawn(move || {
            other_barrier.wait();
            if cancel {
                other.request_cancel(&other_id, CancelMode::Hard).unwrap();
            } else {
                other.enqueue_message(&other_id, "pending", "next").unwrap();
                other
                    .enqueue_message(&other_id, "withdrawn", "later")
                    .unwrap();
                other.withdraw_message(&other_id, "withdrawn").unwrap();
            }
        });
        barrier.wait();
        let fresh = store.begin_fresh_attempt(&id, 0);
        worker.join().unwrap();
        if cancel {
            match fresh {
                Ok(fresh) => assert_eq!(
                    store.cancellation(fresh.fence()).unwrap(),
                    Some(CancelMode::Hard)
                ),
                Err(_) => assert_eq!(
                    store.get(&id).unwrap().lifecycle,
                    foundry_contracts::Lifecycle::Suspended
                ),
            }
        } else {
            assert!(fresh.is_ok());
            assert!(
                store
                    .messages(&id, 0, 10)
                    .unwrap()
                    .iter()
                    .all(|m| m.attempt_id.is_none())
            );
            assert_eq!(store.work_revision(&id).unwrap(), 1);
        }
    }
}
