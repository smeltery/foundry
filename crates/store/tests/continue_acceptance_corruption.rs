mod support_acceptance;
use foundry_store::Store;
use support_acceptance::Fixture;

#[test]
fn corrupt_immutable_rows_and_associations_reject_historical_and_mutating_recovery() {
    for sql in [
        "UPDATE context_continue_acceptances SET request_digest=zeroblob(32)",
        "UPDATE context_continue_acceptances SET record_digest=zeroblob(32)",
        "UPDATE context_continue_acceptances SET committed_ms=committed_ms+1",
        "UPDATE context_continue_acceptances SET event_sequence=event_sequence+1",
        "UPDATE context_continue_acceptances SET intent_body=CAST('{}' AS BLOB)",
        "UPDATE context_continue_messages SET text_digest=zeroblob(32)",
        "UPDATE context_continue_messages SET text_bytes=text_bytes+1",
        "UPDATE context_restores SET admitted_work_revision=admitted_work_revision+1",
        "UPDATE context_restores SET admitted_session_version=admitted_session_version+1",
        "UPDATE context_restores SET receipt_digest=zeroblob(32)",
        "UPDATE context_restores SET delivery_digest=zeroblob(32)",
        "UPDATE context_restores SET continue_digest=zeroblob(32)",
        "UPDATE messages SET text='different bytes'",
        "UPDATE messages SET state='in_flight'",
        "UPDATE events SET data='{}'",
        "UPDATE events SET revision=1",
        "UPDATE events SET kind='message_accepted'",
        "DELETE FROM events",
        "UPDATE context_checkpoints SET record_digest=zeroblob(32)",
        "UPDATE context_checkpoints SET request_digest=zeroblob(32)",
        "UPDATE context_checkpoints SET configuration_digest=zeroblob(32)",
        "UPDATE context_witnesses SET body_digest=zeroblob(32)",
        "UPDATE context_witnesses SET association_digest=zeroblob(32)",
        "DELETE FROM context_witnesses",
        "INSERT INTO events SELECT session_id,sequence+1,id,attempt_id,revision+1,kind,data FROM events",
    ] {
        let mut f = Fixture::new();
        f.accept().unwrap();
        let before = f.store.work_revision(&f.fence.session_id).unwrap();
        f.db().execute_batch(sql).unwrap();
        assert!(f.lookup().is_err(), "{sql}");
        assert!(f.accept().is_err(), "{sql}");
        assert_eq!(f.store.work_revision(&f.fence.session_id).unwrap(), before);
    }
}
#[test]
fn historical_lookup_authenticates_envelopes_without_loading_old_body_or_payload() {
    let mut f = Fixture::new();
    let accepted = f.accept().unwrap();
    // Same-length opaque bytes prove lookup neither parses old W nor hashes old payload.
    // Current-witness read still rejects this corruption through its ordinary validator.
    f.db().execute_batch("UPDATE context_witnesses SET body=zeroblob(length(body)); UPDATE context_checkpoints SET payload=zeroblob(length(payload))").unwrap();
    assert_eq!(f.lookup().unwrap(), Some(accepted));
    assert!(
        f.store
            .read_checkpoint_witness(&f.fence.session_id, &f.input.intent.baseline_checkpoint_id)
            .is_err()
    );
}
#[test]
fn canonical_body_rejects_duplicate_unknown_fields_modes_and_oversize_before_recovery() {
    for mutation in ["duplicate", "unknown", "mode", "deep", "huge", "space"] {
        let mut f = Fixture::new();
        f.accept().unwrap();
        let db = f.db();
        let body: Vec<u8> = db
            .query_row(
                "SELECT intent_body FROM context_continue_acceptances",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let s = String::from_utf8(body).unwrap();
        let changed = match mutation {
            "duplicate" => s.replacen('{', "{\"authorization_binding\":\"bad\",", 1),
            "unknown" => s.replacen('{', "{\"unknown\":0,", 1),
            "mode" => s.replace("queued_instruction", "other_instruction"),
            "deep" => "[".repeat(20) + &"]".repeat(20),
            "huge" => " ".repeat(4097),
            _ => format!(" {s}"),
        };
        db.execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        db.execute(
            "UPDATE context_continue_acceptances SET intent_body=?1",
            [changed.as_bytes()],
        )
        .unwrap();
        assert!(f.lookup().is_err(), "{mutation}");
    }
}
#[test]
fn schema10_migration_is_explicit_preserves_legacy_rows_and_creates_no_acceptance() {
    let mut f = Fixture::new();
    let db = f.db();
    let before: (Vec<u8>, Vec<u8>, Vec<u8>) = db
        .query_row(
            "SELECT begin_digest,continue_digest,receipt_digest FROM context_restores",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    db.execute_batch("DROP TABLE context_continue_acceptances; PRAGMA user_version=10")
        .unwrap();
    f.store = Store::open(f.dir.path().join("db")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        11
    );
    assert!(f.lookup().unwrap().is_none());
    let after: (Vec<u8>, Vec<u8>, Vec<u8>) = db
        .query_row(
            "SELECT begin_digest,continue_digest,receipt_digest FROM context_restores",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(before, after);
    f.accept().unwrap();
}
