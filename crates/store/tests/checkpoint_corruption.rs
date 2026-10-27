mod support_checkpoint;
use foundry_store::Store;
use rusqlite::{Connection, params};
use support_checkpoint::{input, session};
use tempfile::tempdir;

#[test]
fn oversized_stored_identities_are_rejected_before_record_allocation() {
    for field in ["id", "session_id", "attempt_id", "request_id", "parent_id"] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = session(&mut store);
        let fence = store.begin_attempt(&id, 0).unwrap();
        let reference = store
            .commit_opaque_checkpoint(&fence, input(&store, &fence, "a"))
            .unwrap();
        let sql = Connection::open(&path).unwrap();
        sql.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        let large = "x".repeat(131072);
        // New schemas reject the malformed write, even with foreign keys off.
        let mutation = format!("UPDATE context_checkpoints SET {field}=?1");
        assert!(sql.execute(&mutation, [&large]).is_err());
        sql.execute_batch("PRAGMA ignore_check_constraints=ON;")
            .unwrap();
        sql.execute(&mutation, [&large]).unwrap();
        if field == "session_id" {
            assert!(store.checkpoint_head(&id).is_err());
        } else if field == "id" {
            assert!(store.checkpoint_request(&id, "a").is_err());
        } else {
            let error = store
                .read_opaque_checkpoint(&id, &reference.id)
                .err()
                .unwrap();
            assert!(
                error.to_string().contains("stored size limit"),
                "{field}: {error}"
            );
            assert!(store.checkpoint_head(&id).is_err());
        }
    }
}

#[test]
fn malformed_head_id_stale_head_and_missing_head_are_rejected() {
    for case in 0..3 {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = session(&mut store);
        let fence = store.begin_attempt(&id, 0).unwrap();
        let first = store
            .commit_opaque_checkpoint(&fence, input(&store, &fence, "a"))
            .unwrap();
        let mut value = input(&store, &fence, "b");
        value.expected_head = Some(first.id.clone());
        store.commit_opaque_checkpoint(&fence, value).unwrap();
        let sql = Connection::open(&path).unwrap();
        sql.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        match case {
            0 => {
                sql.execute_batch("PRAGMA ignore_check_constraints=ON;")
                    .unwrap();
                sql.execute(
                    "UPDATE context_checkpoint_heads SET checkpoint_id=?1",
                    ["x".repeat(131072)],
                )
                .unwrap();
            }
            1 => {
                sql.execute(
                    "UPDATE context_checkpoint_heads SET checkpoint_id=?1",
                    params![first.id.as_str()],
                )
                .unwrap();
            }
            _ => {
                sql.execute("DELETE FROM context_checkpoint_heads", [])
                    .unwrap();
            }
        }
        assert!(store.checkpoint_head(&id).is_err());
        let mut value = input(&store, &fence, "new");
        if case == 1 {
            value.expected_head = Some(first.id);
        }
        assert!(store.commit_opaque_checkpoint(&fence, value).is_err());
        assert!(store.checkpoint_request(&id, "new").unwrap().is_none());
    }
}

#[test]
fn canonical_request_digest_includes_coverage_and_annotation_for_exact_recovery() {
    let mut store = Store::open(":memory:").unwrap();
    let id = session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    let value = input(&store, &fence, "a");
    let expected = value.request_digest(&fence).unwrap();
    store.commit_opaque_checkpoint(&fence, value).unwrap();
    assert_eq!(
        store
            .checkpoint_request(&id, "a")
            .unwrap()
            .unwrap()
            .request_digest,
        expected
    );
    let mut different = input(&store, &fence, "a");
    different.producer_validation = Some(serde_json::json!({"verified":true}));
    assert_ne!(different.request_digest(&fence).unwrap(), expected);
    different.producer_validation = None;
    different.coverage.message_ids = vec!["different".into()];
    assert_ne!(different.request_digest(&fence).unwrap(), expected);
}
