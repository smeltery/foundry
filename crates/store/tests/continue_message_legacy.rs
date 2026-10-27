mod support_restore;
use foundry_store::{ContinueDecision, ContinueMessageDecision, ContinueMessageInput, Store};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use support_restore::*;

fn hash(domain: &[u8], fields: &[&[u8]]) -> [u8; 32] {
    let mut hash = Sha256::new();
    for field in std::iter::once(domain).chain(fields.iter().copied()) {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field);
    }
    hash.finalize().into()
}
fn bound() -> ContinueMessageInput {
    ContinueMessageInput {
        authorization: continuation("continue"),
        message_id: "message".into(),
        expected_text_digest: Sha256::digest(b"next").into(),
    }
}

#[test]
fn v1_hash_bytes_and_historical_retry_survive_schema_eight_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let (session, checkpoint) = capture(&mut store);
    let owner = begin(&mut store, &session, &checkpoint, "restore");
    store.confirm_restored(&owner, [7; 32]).unwrap();
    store.enqueue_message(&session, "message", "next").unwrap();
    let receipt = match store
        .authorize_continue(&owner, continuation("continue"))
        .unwrap()
    {
        ContinueDecision::Granted { receipt, .. } => receipt,
        _ => panic!("fresh authorization"),
    };
    assert_eq!(receipt.message, None);
    let db = Connection::open(&path).unwrap();
    let (begin, request, stored_receipt): ([u8; 32], [u8; 32], [u8; 32]) = db
        .query_row(
            "SELECT begin_digest,continue_digest,receipt_digest FROM context_restores",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let expected_request = hash(
        b"foundry.restore.continue.v1",
        &[
            owner.restore_id().as_bytes(),
            &begin,
            b"continue",
            &[7; 32],
            &[9; 32],
        ],
    );
    assert_eq!(request, expected_request);
    assert_eq!(
        stored_receipt,
        hash(
            b"foundry.restore.receipt.v1",
            &[
                &request,
                owner.restore_id().as_bytes(),
                receipt.attempt_id.as_bytes(),
                &receipt.session_version.to_be_bytes(),
                &receipt.admitted_work_revision.to_be_bytes()
            ]
        )
    );
    drop(store);
    db.execute_batch("DROP TABLE context_witnesses; ALTER TABLE context_checkpoints DROP COLUMN request_format; DROP TABLE context_continue_messages; DROP INDEX restore_session_identity; PRAGMA user_version=8;").unwrap();
    let mut store = Store::open(&path).unwrap();
    let after: ([u8; 32], [u8; 32]) = db
        .query_row(
            "SELECT continue_digest,receipt_digest FROM context_restores",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(after, (request, stored_receipt));
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM context_continue_messages", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    assert!(
        matches!(store.authorize_continue(&owner, continuation("continue")).unwrap(), ContinueDecision::AlreadyAuthorized(ref actual) if *actual == receipt)
    );
    assert!(store.authorize_continue_message(&owner, bound()).is_err());
    db.execute(
        "INSERT INTO context_continue_messages VALUES(?1,?2,'message',1,?3,4)",
        rusqlite::params![
            owner.restore_id(),
            session,
            Sha256::digest(b"next").as_slice()
        ],
    )
    .unwrap();
    assert!(store.restore_request(&session, "restore").is_err());
    assert!(
        store
            .authorize_continue(&owner, continuation("continue"))
            .is_err()
    );
    assert!(store.authorize_continue_message(&owner, bound()).is_err());
}

#[test]
fn schema_eight_unconsumed_reservation_can_claim_after_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let (session, checkpoint) = capture(&mut store);
    let owner = begin(&mut store, &session, &checkpoint, "restore");
    store.confirm_restored(&owner, [7; 32]).unwrap();
    store.enqueue_message(&session, "message", "next").unwrap();
    drop(store);
    let db = Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE context_witnesses; ALTER TABLE context_checkpoints DROP COLUMN request_format; DROP TABLE context_continue_messages; DROP INDEX restore_session_identity; PRAGMA user_version=8;").unwrap();
    let mut store = Store::open(&path).unwrap();
    assert!(matches!(
        store.authorize_continue_message(&owner, bound()).unwrap(),
        ContinueMessageDecision::Granted { .. }
    ));
    let (other, _) = capture(&mut store);
    assert!(
        db.execute(
            "UPDATE context_continue_messages SET session_id=?1",
            [other]
        )
        .is_err()
    );
}
