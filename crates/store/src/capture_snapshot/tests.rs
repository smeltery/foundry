use super::*;
use crate::{witness::*, witness_facts};

mod checkpoint {
    use crate as foundry_store;
    include!("../../tests/support_checkpoint/mod.rs");
}

#[test]
fn validated_head_facts_and_verification_do_not_reopen_checkpoint_storage() {
    let mut store = Store::open(":memory:").unwrap();
    let session = checkpoint::session(&mut store);
    let fence = store.begin_attempt(&session, 0).unwrap();
    let input = checkpoint::input(&store, &fence, "head");
    store.commit_opaque_checkpoint(&fence, input).unwrap();
    let before = store.capture_snapshot(&fence).unwrap();
    let head = before.head.clone().unwrap();
    let mut body =
        WitnessBodyV1::decode(include_bytes!("../../tests/fixtures/witness-v1.json")).unwrap();
    body.header.cut = before.cut.clone();
    body.records = vec![LogicalRecord::Prompt(PromptWitness {
        session_id: session.clone(),
        attempt_id: fence.attempt_id.clone(),
        text_digest: before.prompt_digest,
        text_bytes: before.prompt_bytes,
    })];
    let tx = store.connection.unchecked_transaction().unwrap();
    // Make any accidental head/body SQL fail, without mutating retained payload bytes.
    tx.execute_batch(
        "ALTER TABLE context_checkpoints RENAME TO unavailable_checkpoints;
        ALTER TABLE context_witnesses RENAME TO unavailable_witnesses;
        ALTER TABLE context_checkpoint_heads RENAME TO unavailable_heads;",
    )
    .unwrap();
    let facts = snapshot_with_head(&tx, &session, head.clone()).unwrap();
    assert_eq!(facts, before);
    witness_facts::verify_evidence(&tx, &session, &body, &facts, &[]).unwrap();
    assert!(snapshot(&tx, &session).is_err());
    assert!(witness_facts::verify_session(&tx, &session, &body).is_err());
    let mut wrong_head = head.clone();
    wrong_head.session_id = uuid::Uuid::new_v4().to_string();
    assert!(snapshot_with_head(&tx, &session, wrong_head).is_err());
    // Metadata reuse must not bypass extraction's durable-fact bounds.
    tx.execute(
        "UPDATE sessions SET body=json_set(body,'$.prompt',?1)",
        ["x".repeat(foundry_contracts::MAX_PROMPT_BYTES + 1)],
    )
    .unwrap();
    assert!(snapshot_with_head(&tx, &session, head).is_err());
    tx.rollback().unwrap();
    assert_eq!(store.capture_snapshot(&fence).unwrap(), before);
}
