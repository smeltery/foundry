mod support_acceptance;
use foundry_store::{witness::*, *};
use sha2::{Digest, Sha256};
use support_acceptance::{Fixture, witness};

fn begin_loading(f: &mut Fixture, checkpoint: &CheckpointRef, request: &str) -> RestoreFence {
    match f
        .store
        .begin_restore(
            &checkpoint.session_id,
            BeginRestoreInput {
                request_id: request.into(),
                checkpoint_id: checkpoint.id.clone(),
                expected_session_version: f.store.get(&checkpoint.session_id).unwrap().version,
                configuration_digest: checkpoint.configuration_digest,
            },
        )
        .unwrap()
    {
        BeginRestoreResult::Started { fence, .. } => fence,
        _ => panic!("expected a fresh loading owner"),
    }
}

fn publish(
    f: &mut Fixture,
    request: &str,
    retained: &[(RestoreContinueIntentV1, ContinueAcceptanceRef)],
) -> CheckpointRef {
    let mut value = witness::input(&f.store, &f.fence, request);
    for (intent, accepted) in retained {
        let logical_index = value.witness.records.len() as u64;
        value
            .checkpoint
            .coverage
            .message_ids
            .push(intent.message_id.clone());
        value
            .witness
            .records
            .push(LogicalRecord::Instruction(InstructionWitness {
                message_id: intent.message_id.clone(),
                sequence: intent.message_sequence,
                attempt_id: accepted.attempt_id.clone(),
                text_digest: intent.message_text_digest,
                text_bytes: intent.message_text_bytes,
                delivery: DeliveryEvidence::ContinueAcceptance(AcceptanceIdentity {
                    restore_id: accepted.restore_id.clone(),
                    digest: accepted.record_digest,
                }),
            }));
        // This fixture tests durable evidence, not native prefix inclusion semantics.
        value.witness.mappings.push(MappingReference {
            logical_index,
            native_index: 0,
            role: MappingRole::Input,
        });
    }
    value.checkpoint.coverage.message_ids.sort();
    f.store
        .finish_with_witnessed_checkpoint(&f.fence, value, witness::terminal())
        .unwrap()
        .checkpoint
}

#[test]
fn two_retained_continuations_restore_without_old_bodies_but_require_oldest_association() {
    let mut f = Fixture::new();
    let c0 = f.input.intent.baseline_checkpoint_id.clone();
    let first = f.accept().unwrap();
    let mut retained = vec![(f.input.intent.clone(), first.clone())];
    let c1 = publish(&mut f, "generation-two", &retained);
    assert_eq!(c1.generation, 2);
    assert_eq!(c1.parent_id.as_ref(), Some(&c0));

    let loading = begin_loading(&mut f, &c1, "load-second-continuation");
    let loaded = f.store.restore_baseline_snapshot(&loading).unwrap();
    assert_eq!(loaded.witness().body().records.len(), 2);
    let baseline_witness_digest = loaded.witness().digest();
    f.store.confirm_restored(&loading, [7; 32]).unwrap();
    let message = f
        .store
        .enqueue_message(&f.fence.session_id, "instruction-two", "second instruction")
        .unwrap();
    let intent = RestoreContinueIntentV1 {
        session_id: c1.session_id.clone(),
        restore_id: loading.restore_id().into(),
        load_id: uuid::Uuid::new_v4().to_string(),
        continuation_id: "continue-second".into(),
        baseline_checkpoint_id: c1.id.clone(),
        baseline_record_digest: c1.record_digest,
        baseline_witness_digest,
        baseline_validation_binding: [94; 32],
        configuration_digest: c1.configuration_digest,
        message_id: message.id,
        message_sequence: message.sequence,
        message_text_digest: Sha256::digest(message.text.as_bytes()).into(),
        message_text_bytes: message.text.len() as u64,
        mode: RestoreContinueMode::QueuedInstruction,
        codec: RestoreContinueCodec::RestoreContinueV2,
        tail_receipt_digest: [43; 32],
    };
    let authorization = match f
        .store
        .authorize_continue_message(
            &loading,
            ContinueMessageInput {
                authorization: ContinueInput {
                    request_id: intent.continuation_id.clone(),
                    configuration_digest: intent.configuration_digest,
                    delivery_digest: intent.delivery_digest().unwrap(),
                },
                message_id: intent.message_id.clone(),
                expected_text_digest: intent.message_text_digest,
            },
        )
        .unwrap()
    {
        ContinueMessageDecision::Granted { permit, receipt } => {
            let (attempt, delivered) = permit.into_delivery();
            assert_eq!(delivered.id, intent.message_id);
            f.fence = attempt;
            receipt
        }
        _ => panic!("expected a new bound continuation"),
    };
    let mut input = ContinueAcceptanceInput {
        authorization,
        intent,
        authorization_binding: [0; 32],
        observed_tail_receipt_digest: [43; 32],
    };
    input.authorization_binding = input.computed_authorization_binding().unwrap();
    let second = f
        .store
        .accept_restore_continuation(&f.fence, input.clone())
        .unwrap();
    assert_ne!(first.attempt_id, second.attempt_id);
    assert_ne!(first.restore_id, second.restore_id);
    retained.push((input.intent.clone(), second.clone()));
    let c2 = publish(&mut f, "generation-three", &retained);
    assert_eq!(c2.generation, 3);
    assert_eq!(c2.parent_id.as_ref(), Some(&c1.id));

    let events_before = f.store.events(&c2.session_id, 0, 10).unwrap();
    let messages_before = f.store.messages(&c2.session_id, 0, 10).unwrap();
    let session_before = f.store.get(&c2.session_id).unwrap();
    let revision_before = f.store.work_revision(&c2.session_id).unwrap();
    assert_eq!(events_before.len(), 2);
    assert_eq!(messages_before.len(), 2);
    let db = f.db();
    assert_eq!(
        db.execute(
            "UPDATE context_witnesses SET body=zeroblob(length(body)) WHERE checkpoint_id IN (?1,?2)",
            rusqlite::params![c0.as_str(), c1.id.as_str()],
        )
        .unwrap(),
        2
    );
    // C0 is beyond the immediate parent; C1 is the immediate parent. Neither body
    // may be decoded through retained acceptance or parent-envelope verification.
    let final_loading = begin_loading(&mut f, &c2, "load-third-generation");
    let snapshot = f.store.restore_baseline_snapshot(&final_loading).unwrap();
    assert_eq!(snapshot.checkpoint().reference().id, c2.id);
    assert_eq!(snapshot.witness().body().records.len(), 3);
    assert_eq!(snapshot.facts().messages.len(), 2);
    assert_eq!(snapshot.events().len(), 2);
    for (index, (intent, accepted)) in retained.iter().enumerate() {
        let LogicalRecord::Instruction(record) = &snapshot.witness().body().records[index + 1]
        else {
            panic!("retained instruction missing")
        };
        assert_eq!(record.message_id, intent.message_id);
        assert_eq!(record.sequence, intent.message_sequence);
        assert_eq!(record.attempt_id, accepted.attempt_id);
        assert_eq!(record.text_digest, intent.message_text_digest);
        assert_eq!(record.text_bytes, intent.message_text_bytes);
        let DeliveryEvidence::ContinueAcceptance(identity) = &record.delivery else {
            panic!("retained acceptance missing")
        };
        assert_eq!(identity.restore_id, accepted.restore_id);
        assert_eq!(identity.digest, accepted.record_digest);
        let event = &snapshot.events()[index];
        assert_eq!(event.id, accepted.event_id);
        assert_eq!(event.sequence, accepted.event_sequence);
        assert_eq!(event.attempt, accepted.attempt_id);
        assert_eq!(event.subject, intent.message_id);
        assert!(event.data_bytes > 0);
    }
    assert_eq!(f.lookup().unwrap(), Some(first));
    assert_eq!(
        f.store.events(&c2.session_id, 0, 10).unwrap(),
        events_before
    );
    assert_eq!(
        f.store.messages(&c2.session_id, 0, 10).unwrap(),
        messages_before
    );
    assert_eq!(f.store.get(&c2.session_id).unwrap(), session_before);
    assert_eq!(
        f.store.work_revision(&c2.session_id).unwrap(),
        revision_before
    );

    // Changing the oldest referenced envelope must fail, even though current C2
    // and its immediate-parent C1 association remain unchanged.
    assert_eq!(
        db.execute(
            "UPDATE context_witnesses SET association_digest=zeroblob(32) WHERE checkpoint_id=?1",
            [c0.as_str()],
        )
        .unwrap(),
        1
    );
    let error = match f.store.restore_baseline_snapshot(&final_loading) {
        Err(error) => error,
        Ok(_) => panic!("corrupt oldest acceptance baseline was accepted"),
    };
    assert!(error.to_string().contains("baseline integrity mismatch"));
    assert_eq!(
        f.store.work_revision(&c2.session_id).unwrap(),
        revision_before
    );
    assert_eq!(f.store.get(&c2.session_id).unwrap(), session_before);
}
