mod support_witness;
use foundry_store::{Store, ToolOutcome, witness::*};
use serde_json::json;
use sha2::{Digest, Sha256};
use support_witness::*;

fn add_mapping(value: &mut foundry_store::WitnessedCheckpointInput, record: LogicalRecord) {
    let role = if matches!(record, LogicalRecord::Instruction(_)) {
        MappingRole::Input
    } else {
        MappingRole::ToolResult
    };
    value.witness.mappings.push(MappingReference {
        logical_index: value.witness.records.len() as u64,
        native_index: 0,
        role,
    });
    value.witness.records.push(record);
}
fn event_identity(event: &foundry_contracts::Event) -> EventIdentity {
    EventIdentity {
        id: event.id.clone(),
        sequence: event.sequence,
        revision: event.revision,
        digest: Sha256::digest(serde_json::to_vec(&event.data).unwrap()).into(),
    }
}
fn codec() -> WitnessCodec {
    WitnessCodec {
        id: "fixture".into(),
        version: "1".into(),
    }
}
fn wire() -> ResultBodyWitness {
    ResultBodyWitness {
        bridge: codec(),
        bytes: 2,
        digest: Sha256::digest(b"{}").into(),
    }
}

#[test]
fn ordinary_message_acceptance_requires_exact_original_event_and_message_identity() {
    for wrong in 0..3 {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("db")).unwrap();
        let session = checkpoint::session(&mut store);
        let fence = store.begin_attempt(&session, 0).unwrap();
        store
            .enqueue_message(&session, "message", "same text")
            .unwrap();
        let message = store.claim_next_message(&fence).unwrap().unwrap();
        let event=store.append_event(&fence,"message-accepted:message",0,"message_accepted",
            json!({"type":"message_accepted","attempt_id":fence.attempt_id,"message_id":"message","receipt":"private-receipt"})).unwrap();
        store.acknowledge_message(&fence, "message").unwrap();
        if wrong == 2 {
            rusqlite::Connection::open(dir.path().join("db"))
                .unwrap()
                .execute("DELETE FROM events WHERE kind='message_accepted'", [])
                .unwrap();
        }
        let mut value = input(&store, &fence, "w");
        value.checkpoint.coverage.message_ids = vec!["message".into()];
        let mut identity = event_identity(&event);
        if wrong == 1 {
            identity.revision += 1;
        }
        add_mapping(
            &mut value,
            LogicalRecord::Instruction(InstructionWitness {
                attempt_id: fence.attempt_id.clone(),
                message_id: "message".into(),
                sequence: message.sequence,
                text_bytes: message.text.len() as u64,
                text_digest: Sha256::digest(message.text.as_bytes()).into(),
                delivery: DeliveryEvidence::OrdinaryEvent(identity),
            }),
        );
        let result = store.finish_with_witnessed_checkpoint(&fence, value, terminal());
        assert_eq!(result.is_err(), wrong != 0);
    }
}

#[test]
fn synthetic_failed_none_and_denied_requests_remain_distinct_durable_facts() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("db")).unwrap();
    let session = checkpoint::session(&mut store);
    let fence = store.begin_attempt(&session, 0).unwrap();
    let input_digest: [u8; 32] = Sha256::digest(b"{}").into();
    let raw = input_digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    for call in ["synthetic", "denied"] {
        store.append_event(&fence,call,0,"tool_request",json!({"type":"tool_call","attempt_id":fence.attempt_id,"call_id":call,"name":"read", "input_encoding":"serde_json.compact.v1","input_bytes":2,"input_sha256":raw})).unwrap();
    }
    store
        .claim_tool(&fence, "synthetic", "read", [9; 32])
        .unwrap();
    store
        .finish_tool(&fence, "synthetic", ToolOutcome::Failed, None)
        .unwrap();
    let denial = store.events(&session, 0, 10).unwrap().remove(1);
    let mut value = input(&store, &fence, "w");
    value.checkpoint.coverage.tool_call_ids = vec!["synthetic".into()];
    add_mapping(
        &mut value,
        LogicalRecord::SyntheticNoEffect(SyntheticResultWitness {
            identity: ToolIdentity {
                attempt_id: fence.attempt_id.clone(),
                call_id: "synthetic".into(),
                tool: "read".into(),
                input_digest: [9; 32],
            },
            failure_branch: codec(),
            wire: wire(),
        }),
    );
    add_mapping(
        &mut value,
        LogicalRecord::DeniedResult(DeniedResultWitness {
            attempt_id: fence.attempt_id.clone(),
            call_id: "denied".into(),
            tool: "read".into(),
            request_event: event_identity(&denial),
            input_encoding: WitnessCodec {
                id: "serde_json.compact.v1".into(),
                version: "1".into(),
            },
            input_digest,
            input_bytes: 2,
            decision_code: "policy-denied".into(),
            policy_revision: "1".into(),
            wire: wire(),
        }),
    );
    let result = store
        .finish_with_witnessed_checkpoint(&fence, value, terminal())
        .unwrap();
    let witness = store
        .read_checkpoint_witness(&session, &result.checkpoint.id)
        .unwrap();
    assert_eq!(witness.body().records.len(), 3);
    assert_eq!(
        store
            .read_opaque_checkpoint(&session, &result.checkpoint.id)
            .unwrap()
            .coverage()
            .tool_call_ids,
        vec!["synthetic"]
    );
}

#[test]
fn selected_event_cap_and_unexplained_delivery_never_publish_partial_witness() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = checkpoint::session(&mut store);
    let fence = store.begin_attempt(&session, 0).unwrap();
    let db = rusqlite::Connection::open(path).unwrap();
    // Fixture creates bounded sanitized historical rows without a matching logical witness.
    db.execute("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1025)
        INSERT INTO events(session_id,sequence,id,attempt_id,revision,kind,data)
        SELECT ?1,x,printf('e-%d',x),?2,0,'message_accepted',json_object('type','message_accepted','attempt_id',?2,'message_id',printf('m-%d',x),'receipt','fixture-receipt') FROM n",
        rusqlite::params![session,fence.attempt_id]).unwrap();
    assert!(
        store
            .finish_with_witnessed_checkpoint(&fence, input(&store, &fence, "w"), terminal())
            .is_err()
    );
    assert!(store.checkpoint_head(&session).unwrap().is_none());
    db.execute("DELETE FROM events WHERE sequence=1025", [])
        .unwrap();
    let error = store
        .finish_with_witnessed_checkpoint(&fence, input(&store, &fence, "byte-limit"), terminal())
        .unwrap_err();
    assert!(error.to_string().contains("byte limit"), "{error}");
}
