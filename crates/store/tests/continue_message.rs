mod support_restore;
use foundry_contracts::{Lifecycle, MessageState};
use foundry_store::{ContinueMessageDecision, ContinueMessageInput, Store};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use support_restore::*;

fn input(id: &str) -> ContinueMessageInput {
    ContinueMessageInput {
        authorization: continuation("deliver"),
        message_id: id.into(),
        expected_text_digest: Sha256::digest(b"next instruction").into(),
    }
}

#[test]
fn exact_claim_receipt_survives_acknowledgement_reopen_and_later_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let (session, checkpoint) = capture(&mut store);
    let owner = begin(&mut store, &session, &checkpoint, "restore");
    store.confirm_restored(&owner, [7; 32]).unwrap();
    let queued = store
        .enqueue_message(&session, "message", "next instruction")
        .unwrap();
    let revision = store.work_revision(&session).unwrap();
    let version = store.get(&session).unwrap().version;
    let (attempt, receipt) = match store
        .authorize_continue_message(&owner, input("message"))
        .unwrap()
    {
        ContinueMessageDecision::Granted { permit, receipt } => {
            let (attempt, message) = permit.into_delivery();
            assert_eq!(message.id, queued.id);
            assert_eq!(message.text, queued.text);
            assert_eq!(message.state, MessageState::InFlight);
            assert_eq!(
                message.attempt_id.as_deref(),
                Some(attempt.attempt_id.as_str())
            );
            assert_eq!(receipt.message.as_ref().unwrap().sequence, queued.sequence);
            (attempt, receipt)
        }
        _ => panic!("first authorization must issue one permit"),
    };
    assert_eq!(receipt.admitted_work_revision, revision + 2);
    assert_eq!(receipt.session_version, version + 1);
    assert!(store.withdraw_message(&session, "message").is_err());
    assert!(store.claim_next_message(&attempt).unwrap().is_none());
    assert!(
        store
            .authorize_continue(&owner, continuation("deliver"))
            .is_err()
    );
    let mut changed = input("message");
    changed.authorization.delivery_digest = [0; 32];
    assert!(store.authorize_continue_message(&owner, changed).is_err());
    store.acknowledge_message(&attempt, "message").unwrap();
    assert_eq!(store.work_revision(&session).unwrap(), revision + 3);
    store
        .finish_attempt(&attempt, Lifecycle::Suspended, None, None)
        .unwrap();
    store
        .begin_attempt(&session, store.get(&session).unwrap().version)
        .unwrap();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    match store
        .authorize_continue_message(&owner, input("message"))
        .unwrap()
    {
        ContinueMessageDecision::AlreadyAuthorized(actual) => assert_eq!(actual, receipt),
        _ => panic!("historical retry must not issue a permit"),
    }
    assert_eq!(
        store
            .restore_request(&session, "restore")
            .unwrap()
            .unwrap()
            .continuation,
        Some(receipt)
    );
    Connection::open(path)
        .unwrap()
        .execute(
            "UPDATE context_restores SET token=?1",
            [uuid::Uuid::new_v4().to_string()],
        )
        .unwrap();
    assert!(
        store
            .authorize_continue_message(&owner, input("message"))
            .is_err()
    );
}

#[test]
fn fifo_withdrawal_bad_content_and_cancellation_do_not_admit() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("store.db")).unwrap();
    let (session, checkpoint) = capture(&mut store);
    let owner = begin(&mut store, &session, &checkpoint, "restore");
    store.confirm_restored(&owner, [7; 32]).unwrap();
    let before = store.get(&session).unwrap();
    store.enqueue_message(&session, "older", "first").unwrap();
    store
        .enqueue_message(&session, "target", "next instruction")
        .unwrap();
    assert!(
        store
            .authorize_continue_message(&owner, input("target"))
            .is_err()
    );
    store.withdraw_message(&session, "older").unwrap();
    let mut bad = input("target");
    bad.expected_text_digest = [0; 32];
    assert!(store.authorize_continue_message(&owner, bad).is_err());
    store.withdraw_message(&session, "target").unwrap();
    assert!(
        store
            .authorize_continue_message(&owner, input("target"))
            .is_err()
    );
    store
        .enqueue_message(&session, "last", "next instruction")
        .unwrap();
    store
        .request_cancel(&session, foundry_contracts::CancelMode::Hard)
        .unwrap();
    assert!(
        store
            .authorize_continue_message(&owner, input("last"))
            .is_err()
    );
    assert_eq!(store.get(&session).unwrap(), before);
    assert_eq!(
        store.work_revision(&session).unwrap(),
        checkpoint.cut.work_revision
    );
}

#[test]
fn context_bounds_do_not_restrict_ordinary_queue_ids() {
    for id in ["x".repeat(129), "id\0control".into()] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("store.db")).unwrap();
        let (session, checkpoint) = capture(&mut store);
        let owner = begin(&mut store, &session, &checkpoint, "restore");
        store.confirm_restored(&owner, [7; 32]).unwrap();
        store
            .enqueue_message(&session, &id, "next instruction")
            .unwrap();
        assert!(
            store
                .authorize_continue_message(&owner, input(&id))
                .is_err()
        );
        store
            .close_restore(&owner, foundry_store::RestoreClose::Closed)
            .unwrap();
        let attempt = store
            .begin_attempt(&session, store.get(&session).unwrap().version)
            .unwrap();
        assert_eq!(store.claim_next_message(&attempt).unwrap().unwrap().id, id);
    }
}

#[test]
fn v2_sidecar_missing_modified_or_attached_without_authorization_is_corruption() {
    for mutation in [
        "DELETE FROM context_continue_messages",
        "UPDATE context_continue_messages SET text_digest=zeroblob(32)",
        "UPDATE context_continue_messages SET sequence=sequence+1",
        "UPDATE context_continue_messages SET text_bytes=text_bytes+1",
        "PRAGMA ignore_check_constraints=ON; UPDATE context_continue_messages SET message_id=printf('%0130d',1)",
        "PRAGMA ignore_check_constraints=ON; UPDATE context_continue_messages SET sequence=-1",
        "UPDATE context_restores SET phase='restored',attempt_id=NULL,continue_request_id=NULL,continue_digest=NULL,delivery_digest=NULL,admitted_session_version=NULL,admitted_work_revision=NULL,receipt_digest=NULL",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        let mut store = Store::open(&path).unwrap();
        let (session, checkpoint) = capture(&mut store);
        let owner = begin(&mut store, &session, &checkpoint, "restore");
        store.confirm_restored(&owner, [7; 32]).unwrap();
        store
            .enqueue_message(&session, "target", "next instruction")
            .unwrap();
        store
            .authorize_continue_message(&owner, input("target"))
            .unwrap();
        Connection::open(path)
            .unwrap()
            .execute_batch(&format!("PRAGMA foreign_keys=OFF; {mutation}"))
            .unwrap();
        assert!(
            store.restore_request(&session, "restore").is_err(),
            "{mutation}"
        );
        assert!(
            store
                .authorize_continue_message(&owner, input("target"))
                .is_err()
        );
        assert!(
            store
                .authorize_continue(&owner, continuation("deliver"))
                .is_err()
        );
    }
}
