mod support_witness;
use foundry_store::{BeginRestoreInput, BeginRestoreResult, Store, witness::*};
use serde_json::json;
use sha2::{Digest, Sha256};
use support_witness::*;

fn fixture(count: usize) -> (tempfile::TempDir, Store, foundry_store::RestoreFence) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("db")).unwrap();
    let session = checkpoint::session(&mut store);
    let attempt = store.begin_attempt(&session, 0).unwrap();
    let mut instructions = Vec::new();
    for n in 0..count {
        let id = format!("message-{n:04}");
        store
            .enqueue_message(&session, &id, "equal text across distinct IDs")
            .unwrap();
        let message = store.claim_next_message(&attempt).unwrap().unwrap();
        let event = store
            .append_event(
                &attempt,
                &format!("accepted-{n:04}"),
                0,
                "message_accepted",
                json!({"type":"message_accepted","attempt_id":attempt.attempt_id,
                "message_id":id,"receipt":"private-receipt"}),
            )
            .unwrap();
        store.acknowledge_message(&attempt, &id).unwrap();
        instructions.push(LogicalRecord::Instruction(InstructionWitness {
            message_id: id,
            sequence: message.sequence,
            attempt_id: attempt.attempt_id.clone(),
            text_digest: Sha256::digest(message.text.as_bytes()).into(),
            text_bytes: message.text.len() as u64,
            delivery: DeliveryEvidence::OrdinaryEvent(EventIdentity {
                id: event.id,
                sequence: event.sequence,
                revision: event.revision,
                digest: Sha256::digest(serde_json::to_vec(&event.data).unwrap()).into(),
            }),
        }));
    }
    let mut value = input(&store, &attempt, "with-messages");
    value.checkpoint.coverage = store.capture_snapshot(&attempt).unwrap().coverage;
    for record in instructions {
        value.witness.mappings.push(MappingReference {
            logical_index: value.witness.records.len() as u64,
            native_index: 0,
            role: MappingRole::Input,
        });
        value.witness.records.push(record);
    }
    let head = store
        .finish_with_witnessed_checkpoint(&attempt, value, terminal())
        .unwrap()
        .checkpoint;
    let request = BeginRestoreInput {
        request_id: "load".into(),
        checkpoint_id: head.id,
        expected_session_version: store.get(&session).unwrap().version,
        configuration_digest: head.configuration_digest,
    };
    let BeginRestoreResult::Started { fence, .. } = store.begin_restore(&session, request).unwrap()
    else {
        panic!("expected new restore");
    };
    (dir, store, fence)
}

#[test]
fn returns_complete_sanitized_event_data_digest_and_exact_byte_length() {
    let (dir, store, fence) = fixture(2);
    let snapshot = store.restore_baseline_snapshot(&fence).unwrap();
    assert_eq!(snapshot.facts().messages.len(), 2);
    assert_eq!(snapshot.events().len(), 2);
    let db = rusqlite::Connection::open(dir.path().join("db")).unwrap();
    for (index, event) in snapshot.events().iter().enumerate() {
        let raw: String = db
            .query_row(
                "SELECT data FROM events WHERE sequence=?1",
                [event.sequence],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(event.data_bytes, raw.len() as u64);
        assert_eq!(
            event.digest,
            <[u8; 32]>::from(Sha256::digest(raw.as_bytes()))
        );
        assert_eq!(event.subject, format!("message-{index:04}"));
        assert_eq!(event.kind, "message_accepted");
        assert_eq!(event.attempt, snapshot.facts().messages[index].attempt_id);
        assert!(event.tool.is_none());
        assert!(event.input_digest.is_none());
    }
    assert_eq!(
        snapshot.facts().messages[0].text_digest,
        snapshot.facts().messages[1].text_digest
    );
    assert_ne!(
        snapshot.facts().messages[0].id,
        snapshot.facts().messages[1].id
    );
}

#[test]
fn combined_facts_and_events_reject_even_when_publication_event_budget_passes() {
    let (_dir, store, fence) = fixture(500);
    let error = match store.restore_baseline_snapshot(&fence) {
        Ok(_) => panic!("combined snapshot must not fit"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("combined evidence byte limit"),
        "{error}"
    );
}

#[test]
fn cancellation_race_returns_a_consistent_snapshot_or_rejects_without_changing_session() {
    let (dir, store, fence) = fixture(20);
    let session = store
        .restore_baseline_snapshot(&fence)
        .unwrap()
        .checkpoint()
        .reference()
        .session_id
        .clone();
    let before = store.get(&session).unwrap();
    let revision = store.work_revision(&session).unwrap();
    let mut canceller = Store::open(dir.path().join("db")).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let peer = barrier.clone();
    let id = session.clone();
    let writer = std::thread::spawn(move || {
        peer.wait();
        canceller
            .request_cancel(&id, foundry_contracts::CancelMode::Graceful)
            .unwrap();
    });
    barrier.wait();
    if let Ok(snapshot) = store.restore_baseline_snapshot(&fence) {
        assert_eq!(snapshot.events().len(), 20);
        assert_eq!(snapshot.facts().messages.len(), 20);
        assert_eq!(snapshot.facts().cut, snapshot.checkpoint().reference().cut);
        assert_eq!(snapshot.witness().body().header.cut, snapshot.facts().cut);
    }
    writer.join().unwrap();
    assert!(store.restore_baseline_snapshot(&fence).is_err());
    assert_eq!(store.get(&session).unwrap(), before);
    assert_eq!(store.work_revision(&session).unwrap(), revision);
}

#[test]
fn concurrent_selection_changes_never_mix_payload_witness_and_facts() {
    for mutation in [
        "UPDATE session_work SET revision=revision+1",
        "UPDATE sessions SET body=json_set(body,'$.version',99)",
        "UPDATE context_restores SET configuration_digest=zeroblob(32)",
        "UPDATE context_restores SET expires_ms=1",
        "DELETE FROM context_checkpoint_heads",
    ] {
        let (dir, store, fence) = fixture(20);
        let baseline = store.restore_baseline_snapshot(&fence).unwrap();
        let payload = baseline.checkpoint().payload().to_vec();
        let digest = baseline.witness().digest();
        let facts = baseline.facts().clone();
        let db = rusqlite::Connection::open(dir.path().join("db")).unwrap();
        let body: Vec<u8> = db
            .query_row("SELECT body FROM context_witnesses", [], |r| r.get(0))
            .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let peer = barrier.clone();
        let writer = std::thread::spawn(move || {
            peer.wait();
            db.execute(mutation, []).unwrap();
            db
        });
        barrier.wait();
        if let Ok(snapshot) = store.restore_baseline_snapshot(&fence) {
            assert_eq!(snapshot.checkpoint().payload(), payload);
            assert_eq!(snapshot.witness().digest(), digest);
            assert_eq!(snapshot.facts(), &facts);
            assert_eq!(snapshot.events().len(), 20);
        }
        let db = writer.join().unwrap();
        assert!(
            store.restore_baseline_snapshot(&fence).is_err(),
            "{mutation}"
        );
        let after: Vec<u8> = db
            .query_row("SELECT body FROM context_witnesses", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, body);
    }
}
