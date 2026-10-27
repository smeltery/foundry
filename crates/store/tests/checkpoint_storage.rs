mod support_checkpoint;
use foundry_contracts::{Lifecycle, Outcome};
use foundry_store::{ContextCheckpointId, MAX_CHECKPOINT_BYTES, Store};
use rusqlite::Connection;
use serde_json::json;
use sha2::{Digest, Sha256};
use support_checkpoint::{input, session};
use tempfile::tempdir;

#[test]
fn opaque_bytes_configuration_and_untrusted_annotation_survive_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let id = session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    let mut value = input(&store, &fence, "first");
    value.producer_validation = Some(json!({"verified": true}));
    let bytes = value.payload.clone();
    let reference = store.commit_opaque_checkpoint(&fence, value).unwrap();
    let sql = Connection::open(&path).unwrap();
    let stored_digest: [u8; 32] = sql
        .query_row(
            "SELECT record_digest FROM context_checkpoints WHERE id=?1",
            [reference.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reference.record_digest, stored_digest);
    // Frozen v1 record encoding: exposing the digest must not redefine it.
    let generation = (reference.generation as i64).to_be_bytes();
    let committed = reference.committed_ms.to_be_bytes();
    let mut legacy = Sha256::new();
    for field in [
        b"foundry.checkpoint.record.v1".as_slice(),
        reference.request_digest.as_slice(),
        reference.id.as_str().as_bytes(),
        &generation,
        &committed,
    ] {
        legacy.update((field.len() as u64).to_be_bytes());
        legacy.update(field);
    }
    assert_eq!(reference.record_digest, <[u8; 32]>::from(legacy.finalize()));
    assert_eq!(
        store
            .capture_snapshot(&fence)
            .unwrap()
            .head
            .unwrap()
            .record_digest,
        stored_digest
    );
    assert_eq!(reference.configuration_digest, [7; 32]);
    assert_eq!(
        reference.payload_digest,
        <[u8; 32]>::from(Sha256::digest(&bytes))
    );
    assert_eq!(reference.cut.work_revision, 1);
    assert!(!format!("{reference:?}").contains("private opaque"));
    assert!(store.events(&id, 0, 10).unwrap().is_empty());
    assert_eq!(store.work_revision(&id).unwrap(), 1);
    drop(store);
    let store = Store::open(&path).unwrap();
    let restored = store.read_opaque_checkpoint(&id, &reference.id).unwrap();
    assert_eq!(restored.payload(), bytes);
    assert_eq!(restored.reference(), &reference);
    assert_eq!(
        restored.producer_validation(),
        Some(&json!({"verified":true}))
    );
    assert_eq!(store.checkpoint_head(&id).unwrap(), Some(reference.clone()));
    assert_eq!(
        store.checkpoint_request(&id, "first").unwrap(),
        Some(reference)
    );
}

#[test]
fn exact_retry_does_not_roll_back_head_and_lookup_survives_fence_completion() {
    let mut store = Store::open(":memory:").unwrap();
    let id = session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    let first = store
        .commit_opaque_checkpoint(&fence, input(&store, &fence, "a"))
        .unwrap();
    let mut next = input(&store, &fence, "b");
    next.expected_head = Some(first.id.clone());
    let second = store.commit_opaque_checkpoint(&fence, next).unwrap();
    assert_eq!(second.generation, 2);
    assert_eq!(second.parent_id, Some(first.id.clone()));
    assert_eq!(
        store
            .commit_opaque_checkpoint(&fence, input(&store, &fence, "a"))
            .unwrap(),
        first
    );
    assert_eq!(store.checkpoint_head(&id).unwrap(), Some(second));
    let mut changed = input(&store, &fence, "a");
    changed.configuration_digest = [8; 32];
    assert!(store.commit_opaque_checkpoint(&fence, changed).is_err());
    let mut changed = input(&store, &fence, "a");
    changed.payload.push(0);
    assert!(store.commit_opaque_checkpoint(&fence, changed).is_err());
    let retry = input(&store, &fence, "a");
    store
        .finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Success), None)
        .unwrap();
    assert!(store.commit_opaque_checkpoint(&fence, retry).is_err());
    assert_eq!(store.checkpoint_request(&id, "a").unwrap(), Some(first));
}

#[test]
fn migration_five_keeps_revision_and_has_no_invented_checkpoint() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let id = session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    drop(store);
    Connection::open(&path).unwrap().execute_batch("DROP TABLE context_witnesses; ALTER TABLE context_checkpoints DROP COLUMN request_format; DROP TABLE context_continue_messages; DROP TABLE context_restores; DROP TABLE context_finalizations; DROP TABLE context_checkpoint_heads; DROP TABLE context_checkpoints; PRAGMA user_version=5;").unwrap();
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.work_revision(&id).unwrap(), 1);
    assert!(store.checkpoint_head(&id).unwrap().is_none());
    store
        .commit_opaque_checkpoint(&fence, input(&store, &fence, "new"))
        .unwrap();
}

#[test]
fn byte_metadata_coverage_and_depth_limits_reject_without_publication() {
    let mut store = Store::open(":memory:").unwrap();
    let id = session(&mut store);
    let fence = store.begin_attempt(&id, 0).unwrap();
    for case in 0..6 {
        let mut value = input(&store, &fence, "limit");
        match case {
            0 => value.payload.clear(),
            1 => value.payload = vec![0; MAX_CHECKPOINT_BYTES + 1],
            2 => value.producer_validation = Some(json!("😀".repeat(4096))),
            3 => value.coverage.message_ids = vec!["x".repeat(65536)],
            4 => value.coverage.message_ids = vec!["same".into(), "same".into()],
            _ => {
                let mut annotation = json!(null);
                for _ in 0..34 {
                    annotation = json!([annotation]);
                }
                value.producer_validation = Some(annotation);
            }
        }
        assert!(store.commit_opaque_checkpoint(&fence, value).is_err());
        assert!(store.checkpoint_head(&id).unwrap().is_none());
        assert!(store.checkpoint_request(&id, "limit").unwrap().is_none());
    }
    let mut value = input(&store, &fence, "max");
    value.payload = vec![0; MAX_CHECKPOINT_BYTES];
    assert_eq!(
        store
            .commit_opaque_checkpoint(&fence, value)
            .unwrap()
            .payload_length,
        MAX_CHECKPOINT_BYTES as u64
    );
    assert!(ContextCheckpointId::parse("invalid").is_err());
}

#[test]
fn corruption_is_rejected_by_every_read_path_and_session_isolation_holds() {
    for mutation in [
        "payload=zeroblob(length(payload))",
        "configuration_digest=zeroblob(32)",
        "metadata='{}'",
        "coverage='{}'",
        "committed_ms=committed_ms+1",
        "request_digest=zeroblob(32)",
        "record_digest=zeroblob(32)",
    ] {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let id = session(&mut store);
        let other = session(&mut store);
        let fence = store.begin_attempt(&id, 0).unwrap();
        let reference = store
            .commit_opaque_checkpoint(&fence, input(&store, &fence, "a"))
            .unwrap();
        assert!(store.read_opaque_checkpoint(&other, &reference.id).is_err());
        assert!(store.checkpoint_request(&other, "a").unwrap().is_none());
        Connection::open(&path)
            .unwrap()
            .execute_batch(&format!("UPDATE context_checkpoints SET {mutation}"))
            .unwrap();
        assert!(
            store.read_opaque_checkpoint(&id, &reference.id).is_err(),
            "{mutation}"
        );
        assert!(store.checkpoint_head(&id).is_err(), "{mutation}");
        assert!(store.capture_snapshot(&fence).is_err(), "{mutation}");
        assert!(store.checkpoint_request(&id, "a").is_err(), "{mutation}");
    }
}
