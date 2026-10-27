mod support_witness;
use foundry_contracts::{CancelMode, ContractError};
use foundry_store::{
    BeginRestoreInput, BeginRestoreResult, CheckpointRef, ContinueInput, RestoreClose,
    RestoreFence, Store, StoreError,
};
use rusqlite::{Connection, params};
use support_witness::*;

fn publish(store: &mut Store, session: &str, request: &str) -> CheckpointRef {
    let fence = store
        .begin_attempt(session, store.get(session).unwrap().version)
        .unwrap();
    let value = input(store, &fence, request);
    store
        .finish_with_witnessed_checkpoint(&fence, value, terminal())
        .unwrap()
        .checkpoint
}

fn begin(store: &mut Store, session: &str, head: &CheckpointRef) -> RestoreFence {
    let request = BeginRestoreInput {
        request_id: "load".into(),
        checkpoint_id: head.id.clone(),
        expected_session_version: store.get(session).unwrap().version,
        configuration_digest: head.configuration_digest,
    };
    match store.begin_restore(session, request).unwrap() {
        BeginRestoreResult::Started { fence, .. } => fence,
        _ => panic!("expected a new loading owner"),
    }
}

fn durable_state(db: &Connection) -> Vec<Vec<rusqlite::types::Value>> {
    let mut result = Vec::new();
    for table in [
        "sessions",
        "session_work",
        "attempts",
        "events",
        "messages",
        "tool_calls",
        "context_checkpoints",
        "context_checkpoint_heads",
        "context_witnesses",
        "context_restores",
    ] {
        let mut query = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let columns = query.column_count();
        let rows = query
            .query_map([], |row| (0..columns).map(|index| row.get(index)).collect())
            .unwrap();
        result.extend(rows.map(Result::unwrap));
    }
    result
}

#[test]
fn loading_and_restored_snapshots_preserve_all_durable_rows_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = checkpoint::session(&mut store);
    let head = publish(&mut store, &session, "capture");
    let fence = begin(&mut store, &session, &head);
    let db = Connection::open(&path).unwrap();
    for restored in [false, true] {
        if restored {
            store.confirm_restored(&fence, [7; 32]).unwrap();
        }
        let before = durable_state(&db);
        let reopened = Store::open(&path).unwrap();
        let snapshot = reopened.restore_baseline_snapshot(&fence).unwrap();
        assert_eq!(snapshot.checkpoint().reference(), &head);
        assert_eq!(snapshot.witness().reference(), &head);
        assert_eq!(snapshot.facts().head.as_ref(), Some(&head));
        assert_eq!(snapshot.facts().cut, head.cut);
        assert_eq!(
            snapshot.checkpoint().payload(),
            b"\0private opaque bytes\xff"
        );
        assert_eq!(snapshot.witness().body().records.len(), 1);
        assert!(snapshot.events().is_empty());
        assert_eq!(durable_state(&db), before);
        assert_eq!(store.get(&session).unwrap().result.as_deref(), Some("done"));
    }
}

#[test]
fn cancelled_expired_closed_and_continuing_owners_cannot_read_baseline() {
    for case in ["cancel", "expire", "closed", "continuing", "wrong-token"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let session = checkpoint::session(&mut store);
        let head = publish(&mut store, &session, "capture");
        let fence = begin(&mut store, &session, &head);
        let db = Connection::open(&path).unwrap();
        match case {
            "cancel" => {
                store
                    .request_cancel(&session, CancelMode::Graceful)
                    .unwrap();
            }
            "expire" => {
                db.execute("UPDATE context_restores SET expires_ms=1", [])
                    .unwrap();
            }
            "closed" => {
                store.close_restore(&fence, RestoreClose::Closed).unwrap();
            }
            "continuing" => {
                store.confirm_restored(&fence, [7; 32]).unwrap();
                store
                    .authorize_continue(
                        &fence,
                        ContinueInput {
                            request_id: "continue".into(),
                            configuration_digest: [7; 32],
                            delivery_digest: [9; 32],
                        },
                    )
                    .unwrap();
            }
            _ => {
                db.execute(
                    "UPDATE context_restores SET token=?1",
                    [uuid::Uuid::new_v4().to_string()],
                )
                .unwrap();
            }
        }
        let before = durable_state(&db);
        assert!(store.restore_baseline_snapshot(&fence).is_err(), "{case}");
        assert_eq!(durable_state(&db), before, "{case}");
    }
}

#[test]
fn changed_selection_and_corrupt_checkpoint_or_witness_fail_readonly() {
    for sql in [
        "UPDATE sessions SET body=json_set(body,'$.version',99)",
        "UPDATE session_work SET revision=revision+1",
        "DELETE FROM context_checkpoint_heads",
        "UPDATE context_restores SET configuration_digest=zeroblob(32)",
        "UPDATE context_checkpoints SET payload=x'01'",
        "UPDATE context_checkpoints SET request_digest=zeroblob(32)",
        "UPDATE context_witnesses SET body=x'7b7d'",
        "UPDATE context_witnesses SET association_digest=zeroblob(32)",
        "DELETE FROM context_witnesses",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let session = checkpoint::session(&mut store);
        let head = publish(&mut store, &session, "capture");
        let fence = begin(&mut store, &session, &head);
        let db = Connection::open(&path).unwrap();
        db.execute(sql, []).unwrap();
        let before = durable_state(&db);
        assert!(store.restore_baseline_snapshot(&fence).is_err(), "{sql}");
        assert_eq!(durable_state(&db), before, "{sql}");
    }
}

#[test]
fn witnessless_legacy_baseline_is_explicitly_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("db")).unwrap();
    let session = checkpoint::session(&mut store);
    let attempt = store.begin_attempt(&session, 0).unwrap();
    let value = checkpoint::input(&store, &attempt, "legacy");
    let head = store
        .finish_with_opaque_checkpoint(&attempt, value, terminal())
        .unwrap()
        .checkpoint;
    let fence = begin(&mut store, &session, &head);
    assert!(matches!(
        store.restore_baseline_snapshot(&fence),
        Err(StoreError::Contract(ContractError::Unsupported(_)))
    ));
}

#[test]
fn reads_only_current_body_and_immediate_parent_association() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = checkpoint::session(&mut store);
    let first = publish(&mut store, &session, "first");
    let second = publish(&mut store, &session, "second");
    let head = publish(&mut store, &session, "third");
    let fence = begin(&mut store, &session, &head);
    let db = Connection::open(&path).unwrap();
    db.execute(
        "DELETE FROM context_witnesses WHERE checkpoint_id=?1",
        [first.id.as_str()],
    )
    .unwrap();
    // The parent's bytes are not loaded or parsed; only its retained envelope is checked.
    db.execute(
        "UPDATE context_witnesses SET body=zeroblob(length(body)) WHERE checkpoint_id=?1",
        [second.id.as_str()],
    )
    .unwrap();
    let snapshot = store.restore_baseline_snapshot(&fence).unwrap();
    assert_eq!(
        snapshot
            .witness()
            .body()
            .header
            .parent
            .as_ref()
            .unwrap()
            .checkpoint_id,
        second.id.as_str()
    );
    db.execute(
        "UPDATE context_witnesses SET association_digest=zeroblob(32) WHERE checkpoint_id=?1",
        [second.id.as_str()],
    )
    .unwrap();
    assert!(store.restore_baseline_snapshot(&fence).is_err());
}

#[test]
fn bounds_reject_oversized_durable_facts_without_mutation() {
    for sql in [
        "UPDATE sessions SET body=json_set(body,'$.prompt',replace(hex(zeroblob(131073)),'0','x'))",
        "UPDATE context_checkpoints SET payload=zeroblob(1048577)",
        "UPDATE context_witnesses SET body=zeroblob(262145)",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let session = checkpoint::session(&mut store);
        let head = publish(&mut store, &session, "capture");
        let fence = begin(&mut store, &session, &head);
        let db = Connection::open(path).unwrap();
        db.execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        db.execute(sql, []).unwrap();
        let before = durable_state(&db);
        assert!(store.restore_baseline_snapshot(&fence).is_err(), "{sql}");
        assert_eq!(durable_state(&db), before);
    }
}

#[test]
fn current_head_cannot_be_replaced_by_an_older_generation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = checkpoint::session(&mut store);
    let first = publish(&mut store, &session, "first");
    let latest = publish(&mut store, &session, "second");
    let fence = begin(&mut store, &session, &latest);
    let db = Connection::open(path).unwrap();
    db.execute(
        "UPDATE context_checkpoint_heads SET checkpoint_id=?1 WHERE session_id=?2",
        params![first.id.as_str(), session],
    )
    .unwrap();
    assert!(store.restore_baseline_snapshot(&fence).is_err());
}
