use crate::{StoreError, witness_events};
use foundry_contracts::ContractError;
use rusqlite::{Connection, params};
use serde_json::json;

fn fixture() -> (Connection, String) {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE events(session_id TEXT, id TEXT, sequence INTEGER,
        revision INTEGER, attempt_id TEXT, kind TEXT, data TEXT)",
    )
    .unwrap();
    (db, uuid::Uuid::new_v4().to_string())
}

fn insert(db: &Connection, attempt: &str, kind: &str, data: serde_json::Value, n: i64) {
    db.execute(
        "INSERT INTO events VALUES('s',?1,?2,0,?3,?4,?5)",
        params![format!("e{n}"), n, attempt, kind, data.to_string()],
    )
    .unwrap();
}

#[test]
fn selected_event_count_includes_every_revision_and_never_truncates() {
    let (db, attempt) = fixture();
    for n in 1..=1025 {
        insert(
            &db,
            &attempt,
            "message_accepted",
            json!({"type":"message_accepted",
            "attempt_id":attempt,"message_id":"same-message","receipt":"private"}),
            n,
        );
    }
    db.execute("UPDATE events SET id='same-event',revision=sequence-1", [])
        .unwrap();
    let error = witness_events::selected(&db, "s").err().unwrap();
    assert!(
        error.to_string().contains("evidence exceeds bounds"),
        "{error}"
    );
    db.execute("DELETE FROM events WHERE sequence=1025", [])
        .unwrap();
    let error = witness_events::selected(&db, "s").err().unwrap();
    assert!(error.to_string().contains("byte limit"), "{error}");
}

#[test]
fn legacy_tool_evidence_is_explicitly_unsupported() {
    let kind = "tool_request";
    let (db, attempt) = fixture();
    insert(
        &db,
        &attempt,
        kind,
        json!({"type":"tool_call","attempt_id":attempt,
        "call_id":"c","name":"read","input":{"private":"raw"}}),
        1,
    );
    assert!(matches!(
        witness_events::selected(&db, "s"),
        Err(StoreError::Contract(ContractError::Unsupported(_)))
    ));
}

#[test]
fn sanitized_tool_request_retains_hash_length_and_original_revision() {
    let (db, attempt) = fixture();
    insert(
        &db,
        &attempt,
        "tool_request",
        json!({"type":"tool_call","attempt_id":attempt,
        "call_id":"c","name":"read","input_encoding":"serde_json.compact.v1",
        "input_bytes":2,"input_sha256":"ab".repeat(32)}),
        7,
    );
    db.execute("UPDATE events SET revision=5", []).unwrap();
    let events = witness_events::selected(&db, "s").unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!((events[0].sequence, events[0].revision), (7, 5));
    assert_eq!(events[0].input_digest, Some([0xab; 32]));
    assert_eq!(events[0].input_bytes, Some(2));
    assert_eq!(events[0].tool.as_deref(), Some("read"));
    assert!(events[0].data_bytes > 2);
}

#[test]
fn event_identity_and_raw_byte_bounds_are_checked_before_evidence_copy() {
    for mutation in [
        "UPDATE events SET id=replace(hex(zeroblob(65)),'0','a')",
        "UPDATE events SET data=replace(hex(zeroblob(1048577)),'0','a')",
        "UPDATE events SET attempt_id='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'",
        "UPDATE events SET revision=-1",
    ] {
        let (db, attempt) = fixture();
        insert(
            &db,
            &attempt,
            "message_accepted",
            json!({"type":"message_accepted",
            "attempt_id":attempt,"message_id":"m","receipt":"private"}),
            1,
        );
        db.execute(mutation, []).unwrap();
        assert!(witness_events::selected(&db, "s").is_err(), "{mutation}");
    }
}
