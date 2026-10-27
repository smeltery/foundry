use foundry_contracts::{CancelMode, CreateSession, Lifecycle};
use foundry_store::{Store, ToolOutcome};
use serde_json::json;
use tempfile::tempdir;

fn request(key: &str) -> CreateSession {
    CreateSession {
        prompt: "offline revision test".into(),
        directory: "/tmp".into(),
        model: None,
        idempotency_key: Some(key.into()),
    }
}

#[test]
fn revisions_count_only_new_admission_and_changed_evidence() {
    let mut store = Store::open(":memory:").unwrap();
    let session = store.create(request("session")).unwrap();
    assert_eq!(store.work_revision(&session.id).unwrap(), 0);
    assert_eq!(store.create(request("session")).unwrap(), session);
    store
        .enqueue_message(&session.id, "m", "instruction")
        .unwrap();
    store
        .enqueue_message(&session.id, "m", "instruction")
        .unwrap();
    store
        .enqueue_message(&session.id, "withdrawn", "unused")
        .unwrap();
    store.withdraw_message(&session.id, "withdrawn").unwrap();
    assert_eq!(store.work_revision(&session.id).unwrap(), 0);
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    assert_eq!(store.work_revision(&session.id).unwrap(), 1);
    store.heartbeat(&fence).unwrap();
    store.get(&session.id).unwrap();
    store.list(10).unwrap();
    store.events(&session.id, 0, 10).unwrap();
    store.messages(&session.id, 0, 10).unwrap();
    store.tool_call(&session.id, "absent").unwrap();
    store.cancellation(&fence).unwrap();
    assert_eq!(store.work_revision(&session.id).unwrap(), 1);

    let event = store
        .append_event(&fence, "event", 0, "text", json!("first"))
        .unwrap();
    assert_eq!(store.work_revision(&session.id).unwrap(), 2);
    assert_eq!(
        store
            .append_event(&fence, "event", 0, "text", json!("first"))
            .unwrap(),
        event
    );
    assert!(
        store
            .append_event(&fence, "event", 0, "text", json!("conflict"))
            .is_err()
    );
    assert_eq!(store.work_revision(&session.id).unwrap(), 2);
    store
        .append_event(&fence, "event", 1, "text", json!("replacement"))
        .unwrap();
    assert_eq!(store.work_revision(&session.id).unwrap(), 3);
    assert!(store.claim_next_message(&fence).unwrap().is_some());
    assert_eq!(store.work_revision(&session.id).unwrap(), 4);
    assert!(store.claim_next_message(&fence).unwrap().is_none());
    assert_eq!(store.work_revision(&session.id).unwrap(), 4);
    let message = store.acknowledge_message(&fence, "m").unwrap();
    assert_eq!(store.work_revision(&session.id).unwrap(), 5);
    assert_eq!(store.acknowledge_message(&fence, "m").unwrap(), message);
    assert!(store.claim_next_message(&fence).unwrap().is_none());
    assert_eq!(store.work_revision(&session.id).unwrap(), 5);

    store.claim_tool(&fence, "call", "read", [1; 32]).unwrap();
    assert_eq!(store.work_revision(&session.id).unwrap(), 6);
    assert!(store.claim_tool(&fence, "call", "read", [1; 32]).is_err());
    assert!(store.claim_tool(&fence, "other", "read", [2; 32]).is_err());
    assert_eq!(store.work_revision(&session.id).unwrap(), 6);
    let receipt = store
        .finish_tool(&fence, "call", ToolOutcome::Failed, None)
        .unwrap();
    assert_eq!(store.work_revision(&session.id).unwrap(), 7);
    assert_eq!(
        store
            .finish_tool(&fence, "call", ToolOutcome::Failed, None)
            .unwrap(),
        receipt
    );
    assert!(
        store
            .finish_tool(&fence, "call", ToolOutcome::Success, Some([3; 32]))
            .is_err()
    );
    assert_eq!(store.work_revision(&session.id).unwrap(), 7);
    store.claim_tool(&fence, "next", "write", [4; 32]).unwrap();
    store
        .finish_tool(&fence, "next", ToolOutcome::Uncertain, None)
        .unwrap();
    assert_eq!(store.work_revision(&session.id).unwrap(), 9);
    store
        .finish_tool(&fence, "next", ToolOutcome::Uncertain, None)
        .unwrap();
    assert_eq!(store.work_revision(&session.id).unwrap(), 9);
    store.request_cancel(&session.id, CancelMode::Hard).unwrap();
    store
        .request_cancel(&session.id, CancelMode::Graceful)
        .unwrap();
    assert!(store.claim_next_message(&fence).unwrap().is_none());
    assert!(
        store
            .claim_tool(&fence, "cancelled", "read", [0; 32])
            .is_err()
    );
    store
        .finish_attempt(&fence, Lifecycle::Suspended, None, None)
        .unwrap();
    assert_eq!(store.work_revision(&session.id).unwrap(), 9);
}

#[test]
fn eventless_admission_survives_owner_loss_and_reopen() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = store.create(request("eventless")).unwrap();
    let before = store.work_revision(&session.id).unwrap();
    store.begin_attempt(&session.id, 0).unwrap();
    drop(store); // Owner disappears without appending any event or terminal state.
    let mut reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.work_revision(&session.id).unwrap(), before + 1);
    assert!(reopened.events(&session.id, 0, 10).unwrap().is_empty());
    assert!(reopened.begin_attempt(&session.id, 1).is_err());
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE attempts SET expires_ms=0 WHERE session_id=?1",
            [&session.id],
        )
        .unwrap();
    let recovered = reopened.recover_expired().unwrap().remove(0);
    assert_eq!(reopened.work_revision(&session.id).unwrap(), before + 1);
    reopened
        .begin_attempt(&session.id, recovered.version)
        .unwrap();
    assert_eq!(reopened.work_revision(&session.id).unwrap(), before + 2);
}

#[test]
fn migration_four_preserves_existing_rows_and_initializes_only_revision_baseline() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = store.create(request("migration")).unwrap();
    let other = store.create(request("other")).unwrap();
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    store
        .enqueue_message(&session.id, "message", "pending input")
        .unwrap();
    let message = store.claim_next_message(&fence).unwrap().unwrap();
    let tool = store.claim_tool(&fence, "call", "write", [1; 32]).unwrap();
    let event = store
        .append_event(&fence, "event", 0, "text", json!("old"))
        .unwrap();
    let running = store.get(&session.id).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch("DROP TABLE context_continue_acceptances; DROP TABLE context_witnesses; ALTER TABLE context_checkpoints DROP COLUMN request_format; DROP TABLE context_continue_messages; DROP TABLE context_restores; DROP TABLE context_finalizations; DROP TABLE context_checkpoint_heads; DROP TABLE context_checkpoints; DROP TABLE session_work; PRAGMA user_version=4;")
        .unwrap();
    drop(connection);
    let mut migrated = Store::open(&path).unwrap();
    assert_eq!(migrated.get(&session.id).unwrap(), running);
    assert_eq!(migrated.get(&other.id).unwrap(), other);
    assert_eq!(
        migrated.messages(&session.id, 0, 10).unwrap(),
        vec![message]
    );
    assert_eq!(migrated.tool_call(&session.id, "call").unwrap(), Some(tool));
    assert_eq!(migrated.events(&session.id, 0, 10).unwrap(), vec![event]);
    assert_eq!(migrated.work_revision(&session.id).unwrap(), 0);
    assert_eq!(migrated.work_revision(&other.id).unwrap(), 0);
    assert!(migrated.claim_tool(&fence, "new", "read", [2; 32]).is_err());
    assert!(migrated.claim_next_message(&fence).unwrap().is_none());
    migrated
        .append_event(&fence, "new-event", 0, "text", json!("new"))
        .unwrap();
    assert_eq!(migrated.work_revision(&session.id).unwrap(), 1);
    drop(migrated);
    let reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.work_revision(&session.id).unwrap(), 1);
    let connection = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        11
    );
}

#[test]
fn foreign_and_expired_fences_cannot_change_either_sessions_revision() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let a = store.create(request("a")).unwrap();
    let b = store.create(request("b")).unwrap();
    let fence = store.begin_attempt(&a.id, 0).unwrap();
    let mut forged = fence.clone();
    forged.session_id = b.id.clone();
    for invalid in [&forged, &fence] {
        if invalid.session_id == a.id {
            rusqlite::Connection::open(&path)
                .unwrap()
                .execute("UPDATE attempts SET expires_ms=0", [])
                .unwrap();
        }
        assert!(
            store
                .append_event(invalid, "bad", 0, "text", json!(null))
                .is_err()
        );
        assert!(store.claim_next_message(invalid).is_err());
        assert!(store.acknowledge_message(invalid, "bad").is_err());
        assert!(store.claim_tool(invalid, "bad", "read", [0; 32]).is_err());
        assert!(
            store
                .finish_tool(invalid, "bad", ToolOutcome::Failed, None)
                .is_err()
        );
        assert_eq!(store.work_revision(&a.id).unwrap(), 1);
        assert_eq!(store.work_revision(&b.id).unwrap(), 0);
    }
    assert!(store.work_revision("absent").is_err());
    assert_eq!(store.list(10).unwrap().len(), 2);
}
