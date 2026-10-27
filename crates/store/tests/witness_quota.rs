mod support_witness;
use foundry_store::{MAX_CHECKPOINT_BYTES, Store};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use support_witness::*;

#[test]
fn legacy_writer_charges_witness_and_finalization_and_exact_retry_at_quota_is_free() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = checkpoint::session(&mut store);
    let fence = store.begin_attempt(&session, 0).unwrap();
    let mut root = input(&store, &fence, "root");
    root.checkpoint.payload = vec![0; MAX_CHECKPOINT_BYTES];
    root.witness.header.payload_digest = Sha256::digest(&root.checkpoint.payload).into();
    root.witness.header.payload_length = MAX_CHECKPOINT_BYTES as u64;
    let root_request = root
        .finalization_request_digest(&fence, &terminal())
        .unwrap();
    let mut head = store
        .finish_with_witnessed_checkpoint(&fence, root, terminal())
        .unwrap()
        .checkpoint;
    let fence = store
        .begin_attempt(&session, store.get(&session).unwrap().version)
        .unwrap();
    for n in 0..254 {
        store.heartbeat(&fence).unwrap();
        let mut value = checkpoint::input(&store, &fence, &format!("legacy-{n}"));
        value.expected_head = Some(head.id.clone());
        value.payload = vec![0; MAX_CHECKPOINT_BYTES];
        head = store.commit_opaque_checkpoint(&fence, value).unwrap();
    }
    let db = Connection::open(&path).unwrap();
    // Independent contract arithmetic over actual stored column bytes, including
    // the schema10 integer, witness envelope and schema7 terminal receipt.
    let current:i64=db.query_row("SELECT SUM(length(payload)+length(CAST(metadata AS BLOB))+length(CAST(coverage AS BLOB))
        +length(id)+length(session_id)+length(attempt_id)+length(request_id)+COALESCE(length(parent_id),0)+152
        +COALESCE((SELECT 76 FROM context_finalizations f WHERE f.checkpoint_id=c.id),0)
        +COALESCE((SELECT length(body)+184 FROM context_witnesses w WHERE w.checkpoint_id=c.id),0)) FROM context_checkpoints c",[],|r|r.get(0)).unwrap();
    let envelope:(i64,i64,i64)=db.query_row("SELECT length(CAST(metadata AS BLOB)),length(CAST(coverage AS BLOB)),length(request_id) FROM context_checkpoints WHERE id=?1",[head.id.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    let request = "exact-boundary";
    let overhead = envelope.0 + envelope.1 + 36 * 4 + request.len() as i64 + 152;
    let payload_bytes = (256 * 1024 * 1024 - current - overhead) as usize;
    assert!(payload_bytes > 0 && payload_bytes < MAX_CHECKPOINT_BYTES);
    let make = |store: &Store, size| {
        let mut value = checkpoint::input(store, &fence, request);
        value.expected_head = Some(head.id.clone());
        value.payload = vec![0; size];
        value
    };
    assert!(
        store
            .commit_opaque_checkpoint(&fence, make(&store, payload_bytes + 1))
            .unwrap_err()
            .to_string()
            .contains("quota")
    );
    let exact = store
        .commit_opaque_checkpoint(&fence, make(&store, payload_bytes))
        .unwrap();
    assert_eq!(
        store
            .commit_opaque_checkpoint(&fence, make(&store, payload_bytes))
            .unwrap(),
        exact
    );
    assert!(
        store
            .finished_checkpoint_request(&session, "root", root_request)
            .unwrap()
            .is_some()
    );
}
