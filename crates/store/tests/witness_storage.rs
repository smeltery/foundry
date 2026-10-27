mod support_witness;
use foundry_store::Store;
use rusqlite::Connection;
use support_witness::*;

#[test]
fn witnessed_finalization_reopens_exactly_and_keeps_legacy_hashes_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = checkpoint::session(&mut store);
    let fence = store.begin_attempt(&session, 0).unwrap();
    let value = input(&store, &fence, "witness");
    let expected = value
        .finalization_request_digest(&fence, &terminal())
        .unwrap();
    assert_ne!(
        expected,
        value
            .checkpoint
            .finalization_request_digest(&fence, &terminal())
            .unwrap()
    );
    let golden_body = value.witness.encode().unwrap();
    let result = store
        .finish_with_witnessed_checkpoint(&fence, value, terminal())
        .unwrap();
    assert_eq!(result.checkpoint.request_digest, expected);
    drop(store);
    let store = Store::open(&path).unwrap();
    let witness = store
        .read_checkpoint_witness(&session, &result.checkpoint.id)
        .unwrap();
    assert_eq!(witness.digest(), golden_body.digest());
    assert_eq!(
        witness.body().encode().unwrap().bytes(),
        golden_body.bytes()
    );
    assert_eq!(witness.reference(), &result.checkpoint);
    assert_eq!(
        store
            .finished_checkpoint_request(&session, "witness", expected)
            .unwrap(),
        Some(result.clone())
    );
    let db = Connection::open(path).unwrap();
    assert_eq!(
        db.query_row("SELECT request_format FROM context_checkpoints", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        2
    );
    let (body_digest, length): ([u8; 32], i64) = db
        .query_row(
            "SELECT body_digest,length(body) FROM context_witnesses",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let fields: Vec<Vec<u8>> = db
        .query_row(
            "SELECT session_id,attempt_id,request_id,COALESCE(parent_id,''),metadata,coverage,
            configuration_digest,payload_digest,length(payload) FROM context_checkpoints",
            [],
            |row| {
                let mut fields = Vec::new();
                for index in 0..6 {
                    fields.push(row.get::<_, String>(index)?.into_bytes());
                }
                fields.push(row.get::<_, Vec<u8>>(6)?);
                fields.push(row.get::<_, Vec<u8>>(7)?);
                fields.push((row.get::<_, i64>(8)? as u64).to_be_bytes().to_vec());
                fields.push(1_u64.to_be_bytes().to_vec());
                fields.push(body_digest.to_vec());
                fields.push((length as u64).to_be_bytes().to_vec());
                Ok(fields)
            },
        )
        .unwrap();
    let fields: Vec<&[u8]> = fields.iter().map(Vec::as_slice).collect();
    assert_eq!(expected, digest(b"foundry.checkpoint.request.v2", &fields));
    assert_eq!(body_digest, golden_body.digest());
    assert_eq!(length, golden_body.bytes().len() as i64);
}

#[test]
fn current_witness_reads_only_one_parent_envelope_not_grandparent_body() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = checkpoint::session(&mut store);
    let mut ids = Vec::new();
    for n in 0..3 {
        let fence = store
            .begin_attempt(&session, store.get(&session).unwrap().version)
            .unwrap();
        let value = input(&store, &fence, &format!("checkpoint-{n}"));
        let result = store
            .finish_with_witnessed_checkpoint(&fence, value, terminal())
            .unwrap();
        assert_eq!(result.checkpoint.generation, n + 1);
        ids.push(result.checkpoint.id);
    }
    let db = Connection::open(path).unwrap();
    db.execute(
        "UPDATE context_witnesses SET body=zeroblob(length(body)) WHERE checkpoint_id=?1",
        [ids[0].as_str()],
    )
    .unwrap();
    assert!(store.read_checkpoint_witness(&session, &ids[2]).is_ok());
    assert!(store.read_checkpoint_witness(&session, &ids[0]).is_err());
    db.execute(
        "DELETE FROM context_witnesses WHERE checkpoint_id=?1",
        [ids[1].as_str()],
    )
    .unwrap();
    assert!(store.read_checkpoint_witness(&session, &ids[2]).is_err());
}

#[test]
fn format_missing_child_and_corrupt_association_never_fall_back() {
    for mutation in [
        "UPDATE context_checkpoints SET request_format=1",
        "PRAGMA ignore_check_constraints=ON; UPDATE context_checkpoints SET request_format=3",
        "DELETE FROM context_witnesses",
        "UPDATE context_witnesses SET body=zeroblob(length(body))",
        "UPDATE context_witnesses SET body_digest=zeroblob(32)",
        "UPDATE context_witnesses SET checkpoint_record_digest=zeroblob(32)",
        "UPDATE context_witnesses SET association_digest=zeroblob(32)",
        "UPDATE context_witnesses SET checkpoint_generation=99",
        "PRAGMA ignore_check_constraints=ON; UPDATE context_witnesses SET body=zeroblob(262145)",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let session = checkpoint::session(&mut store);
        let fence = store.begin_attempt(&session, 0).unwrap();
        let result = store
            .finish_with_witnessed_checkpoint(&fence, input(&store, &fence, "w"), terminal())
            .unwrap();
        Connection::open(path)
            .unwrap()
            .execute_batch(mutation)
            .unwrap();
        assert!(store.checkpoint_head(&session).is_err(), "{mutation}");
        assert!(store.checkpoint_request(&session, "w").is_err());
        assert!(
            store
                .read_opaque_checkpoint(&session, &result.checkpoint.id)
                .is_err()
        );
        assert!(
            store
                .read_checkpoint_witness(&session, &result.checkpoint.id)
                .is_err()
        );
        assert!(
            store
                .finished_checkpoint_request(&session, "w", result.checkpoint.request_digest)
                .is_err()
        );
    }
}

#[test]
fn schema_nine_migration_preserves_legacy_request_and_record_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = checkpoint::session(&mut store);
    let fence = store.begin_attempt(&session, 0).unwrap();
    let reference = store
        .commit_opaque_checkpoint(&fence, checkpoint::input(&store, &fence, "legacy"))
        .unwrap();
    drop(store);
    let db = Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE context_witnesses; ALTER TABLE context_checkpoints DROP COLUMN request_format; PRAGMA user_version=9").unwrap();
    let store = Store::open(path).unwrap();
    assert_eq!(
        store.checkpoint_head(&session).unwrap(),
        Some(reference.clone())
    );
    assert!(
        store
            .read_checkpoint_witness(&session, &reference.id)
            .is_err()
    );
    assert_eq!(
        db.query_row("SELECT request_format FROM context_checkpoints", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM context_witnesses", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
