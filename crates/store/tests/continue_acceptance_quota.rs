mod support_acceptance;
use foundry_store::{MAX_CHECKPOINT_BYTES, OpaqueCheckpointInput};
use support_acceptance::{Fixture, witness};

fn charge(f: &Fixture) -> i64 {
    f.db().query_row("SELECT
        (SELECT SUM(length(payload)+length(CAST(metadata AS BLOB))+length(CAST(coverage AS BLOB))
        +length(CAST(id AS BLOB))+length(CAST(session_id AS BLOB))+length(CAST(attempt_id AS BLOB))
        +length(CAST(request_id AS BLOB))+COALESCE(length(CAST(parent_id AS BLOB)),0)+152
        +COALESCE((SELECT 76 FROM context_finalizations WHERE checkpoint_id=c.id),0)
        +COALESCE((SELECT length(body)+184 FROM context_witnesses WHERE checkpoint_id=c.id),0)) FROM context_checkpoints c)
        +(SELECT SUM(length(a.intent_body)+116+length(CAST(e.session_id AS BLOB))+length(CAST(e.attempt_id AS BLOB))
        +length(CAST(e.id AS BLOB))+length(CAST(e.kind AS BLOB))+length(CAST(e.data AS BLOB))+16)
        FROM context_continue_acceptances a JOIN context_restores r ON r.id=a.restore_id
        JOIN events e ON e.session_id=r.session_id AND e.sequence=a.event_sequence)",[],|r|r.get(0)).unwrap()
}
fn next(f: &Fixture, request: &str) -> OpaqueCheckpointInput {
    let mut input = witness::checkpoint::input(&f.store, &f.fence, request);
    input.expected_head = f
        .store
        .checkpoint_head(&f.fence.session_id)
        .unwrap()
        .map(|v| v.id);
    input.coverage.message_ids = vec![f.input.intent.message_id.clone()];
    input
}
#[test]
fn all_writers_charge_acceptance_and_matched_event_and_zero_growth_retry_is_free() {
    let mut f = Fixture::new();
    let accepted = f.accept().unwrap();
    let root = f.input.intent.baseline_checkpoint_id.clone();
    for n in 0..255 {
        f.store.heartbeat(&f.fence).unwrap();
        let mut input = next(&f, &format!("fill-{n}"));
        input.payload = vec![0; MAX_CHECKPOINT_BYTES];
        f.store.commit_opaque_checkpoint(&f.fence, input).unwrap();
    }
    let mut probe = next(&f, "size-probe");
    probe.payload = vec![0; 1];
    let previous = charge(&f);
    f.store.commit_opaque_checkpoint(&f.fence, probe).unwrap();
    let overhead = charge(&f) - previous - 1;
    let remaining = 256 * 1024 * 1024 - charge(&f) - overhead;
    assert!(remaining > 0 && remaining < MAX_CHECKPOINT_BYTES as i64);
    let make = |f: &Fixture, bytes: usize| {
        let mut v = next(f, "exact-fill");
        v.payload = vec![0; bytes];
        v
    };
    let excessive = make(&f, remaining as usize + 1);
    assert!(
        f.store
            .commit_opaque_checkpoint(&f.fence, excessive)
            .unwrap_err()
            .to_string()
            .contains("quota")
    );
    let exact = make(&f, remaining as usize);
    f.store.commit_opaque_checkpoint(&f.fence, exact).unwrap();
    assert_eq!(charge(&f), 256 * 1024 * 1024);
    assert_eq!(f.accept().unwrap(), accepted);
    assert_eq!(f.lookup().unwrap(), Some(accepted));
    assert_eq!(f.input.intent.baseline_checkpoint_id, root);
    let input = next(&f, "opaque-finish");
    assert!(
        f.store
            .finish_with_opaque_checkpoint(&f.fence, input, witness::terminal())
            .unwrap_err()
            .to_string()
            .contains("quota")
    );
    // Model a fresh acceptance at the same byte boundary using retained charge fixtures.
    // The nonbaseline filler length changes only the quota arithmetic under test.
    let db = f.db();
    db.execute_batch("DELETE FROM context_continue_acceptances; DELETE FROM events; UPDATE messages SET state='in_flight'; UPDATE context_checkpoints SET payload=zeroblob(2) WHERE request_id='size-probe'").unwrap();
    let before = f.store.work_revision(&f.fence.session_id).unwrap();
    assert!(f.accept().unwrap_err().to_string().contains("quota"));
    assert_eq!(f.store.work_revision(&f.fence.session_id).unwrap(), before);
    assert!(f.lookup().unwrap().is_none());
    db.execute_batch(
        "UPDATE context_checkpoints SET payload=zeroblob(1) WHERE request_id='size-probe'",
    )
    .unwrap();
    f.accept().unwrap();
    assert_eq!(charge(&f), 256 * 1024 * 1024);
}

#[test]
fn missing_acceptance_event_cannot_be_inner_joined_out_of_any_writer_quota() {
    let mut f = Fixture::new();
    f.accept().unwrap();
    let input = next(&f, "next");
    f.db().execute_batch("DELETE FROM events").unwrap();
    // Refresh cut only; the historical acceptance is still corrupt and must reject.
    let mut input = input;
    input.cut = f.store.checkpoint_cut(&f.fence).unwrap();
    assert!(f.store.commit_opaque_checkpoint(&f.fence, input).is_err());
}
