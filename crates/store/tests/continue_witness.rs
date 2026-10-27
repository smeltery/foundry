mod support_acceptance;
use foundry_store::{witness::*, *};
use support_acceptance::{Fixture, witness};

fn retained(f: &Fixture, accepted: &ContinueAcceptanceRef) -> WitnessedCheckpointInput {
    let mut value = witness::input(&f.store, &f.fence, "continued");
    let intent = &f.input.intent;
    value.checkpoint.coverage.message_ids = vec![intent.message_id.clone()];
    value
        .witness
        .records
        .push(LogicalRecord::Instruction(InstructionWitness {
            attempt_id: accepted.attempt_id.clone(),
            message_id: intent.message_id.clone(),
            sequence: intent.message_sequence,
            text_digest: intent.message_text_digest,
            text_bytes: intent.message_text_bytes,
            delivery: DeliveryEvidence::ContinueAcceptance(AcceptanceIdentity {
                restore_id: accepted.restore_id.clone(),
                digest: accepted.record_digest,
            }),
        }));
    value.witness.mappings.push(MappingReference {
        logical_index: 1,
        native_index: 0,
        role: MappingRole::Input,
    });
    value
}

fn begin_loading(f: &mut Fixture, checkpoint: &CheckpointRef) -> RestoreFence {
    match f
        .store
        .begin_restore(
            &f.fence.session_id,
            BeginRestoreInput {
                request_id: "next-load".into(),
                checkpoint_id: checkpoint.id.clone(),
                expected_session_version: f.store.get(&f.fence.session_id).unwrap().version,
                configuration_digest: [7; 32],
            },
        )
        .unwrap()
    {
        BeginRestoreResult::Started { fence, .. } => fence,
        _ => panic!("new loading owner"),
    }
}

#[test]
fn continued_instruction_survives_publication_and_loading_without_ancestor_bodies() {
    let mut f = Fixture::new();
    let accepted = f.accept().unwrap();
    let value = retained(&f, &accepted);
    let child = f
        .store
        .finish_with_witnessed_checkpoint(&f.fence, value, witness::terminal())
        .unwrap()
        .checkpoint;
    let owner = begin_loading(&mut f, &child);
    let before = f.store.work_revision(&f.fence.session_id).unwrap();
    // Historical acceptance authenticates its envelope, without decoding this old body.
    f.db()
        .execute(
            "UPDATE context_witnesses SET body=zeroblob(length(body)) WHERE checkpoint_id=?1",
            [f.input.intent.baseline_checkpoint_id.as_str()],
        )
        .unwrap();
    let snapshot = f.store.restore_baseline_snapshot(&owner).unwrap();
    assert_eq!(snapshot.witness().body().records.len(), 2);
    assert_eq!(snapshot.facts().messages.len(), 1);
    assert_eq!(snapshot.events().len(), 1);
    assert_eq!(snapshot.events()[0].id, accepted.event_id);
    assert_eq!(snapshot.events()[0].subject, f.input.intent.message_id);
    assert_eq!(snapshot.events()[0].attempt, accepted.attempt_id);
    assert!(snapshot.events()[0].data_bytes > 0);
    assert_eq!(f.store.work_revision(&f.fence.session_id).unwrap(), before);
    assert_eq!(f.lookup().unwrap(), Some(accepted));
    // The current selected body still receives full validation.
    f.db()
        .execute(
            "UPDATE context_witnesses SET body=zeroblob(length(body)) WHERE checkpoint_id=?1",
            [child.id.as_str()],
        )
        .unwrap();
    assert!(f.store.restore_baseline_snapshot(&owner).is_err());
}

#[test]
fn altered_acceptance_identity_never_publishes() {
    for wrong in 0..4 {
        let mut f = Fixture::new();
        let accepted = f.accept().unwrap();
        let mut value = retained(&f, &accepted);
        let LogicalRecord::Instruction(instruction) = &mut value.witness.records[1] else {
            unreachable!()
        };
        match wrong {
            0 => {
                let DeliveryEvidence::ContinueAcceptance(identity) = &mut instruction.delivery
                else {
                    unreachable!()
                };
                identity.digest = accepted.request_digest;
            }
            1 => instruction.attempt_id = uuid::Uuid::new_v4().to_string(),
            2 => instruction.message_id = "other-message".into(),
            _ => instruction.sequence += 1,
        }
        assert!(
            f.store
                .finish_with_witnessed_checkpoint(&f.fence, value, witness::terminal())
                .is_err()
        );
        assert_eq!(
            f.store
                .checkpoint_head(&f.fence.session_id)
                .unwrap()
                .unwrap()
                .id,
            f.input.intent.baseline_checkpoint_id
        );
    }
}

#[test]
fn missing_or_altered_acceptance_event_fails_loading() {
    for mutation in [
        "DELETE FROM context_continue_acceptances",
        "DELETE FROM events WHERE kind='continue_accepted'",
        "UPDATE events SET revision=1 WHERE kind='continue_accepted'",
        "UPDATE events SET data=json_set(data,'$.message_id','wrong') WHERE kind='continue_accepted'",
    ] {
        let mut f = Fixture::new();
        let accepted = f.accept().unwrap();
        let value = retained(&f, &accepted);
        let child = f
            .store
            .finish_with_witnessed_checkpoint(&f.fence, value, witness::terminal())
            .unwrap()
            .checkpoint;
        let owner = begin_loading(&mut f, &child);
        f.db().execute_batch(mutation).unwrap();
        assert!(
            f.store.restore_baseline_snapshot(&owner).is_err(),
            "{mutation}"
        );
    }
}

#[test]
fn orphan_or_changed_event_rejects_publication_even_with_a_fresh_cut() {
    for mutation in [
        "DELETE FROM context_continue_acceptances",
        "UPDATE events SET revision=1 WHERE kind='continue_accepted'",
        "UPDATE events SET data=json_set(data,'$.message_id','wrong') WHERE kind='continue_accepted'",
    ] {
        let mut f = Fixture::new();
        let accepted = f.accept().unwrap();
        f.db().execute_batch(mutation).unwrap();
        // Build from the changed durable cut, so cut mismatch cannot mask decoder failure.
        let value = retained(&f, &accepted);
        let before = f.store.work_revision(&f.fence.session_id).unwrap();
        let session = f.store.get(&f.fence.session_id).unwrap();
        let error = f
            .store
            .finish_with_witnessed_checkpoint(&f.fence, value, witness::terminal())
            .unwrap_err();
        assert!(
            error.to_string().contains("acceptance"),
            "{mutation}: {error}"
        );
        assert_eq!(f.store.get(&f.fence.session_id).unwrap(), session);
        assert_eq!(f.store.work_revision(&f.fence.session_id).unwrap(), before);
        assert_eq!(
            f.store
                .checkpoint_head(&f.fence.session_id)
                .unwrap()
                .unwrap()
                .id,
            f.input.intent.baseline_checkpoint_id
        );
    }
}
