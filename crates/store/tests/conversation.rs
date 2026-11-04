use foundry_contracts::{CreateSession, Lifecycle, Outcome};
use foundry_store::{AttemptFence, Store};
use serde_json::{Value, json};
use tempfile::tempdir;

fn create(store: &mut Store) -> foundry_contracts::Session {
    store
        .create(CreateSession {
            prompt: "opening".into(),
            directory: "/tmp".into(),
            model: None,
            idempotency_key: None,
        })
        .unwrap()
}

fn text(fence: &AttemptFence, text: &str) -> Value {
    json!({"type":"text","attempt_id":fence.attempt_id,"text":text})
}

fn deliver(store: &mut Store, fence: &AttemptFence, id: &str) {
    let claimed = store.claim_next_message(fence).unwrap().unwrap();
    assert_eq!(claimed.id, id);
    store
        .append_event(
            fence,
            &format!("message-accepted:{id}"),
            0,
            "message_accepted",
            json!({"type":"message_accepted","attempt_id":fence.attempt_id,
                "message_id":id,"receipt":"r"}),
        )
        .unwrap();
    store.acknowledge_message(fence, id).unwrap();
}

fn kinds(store: &Store, session_id: &str, after: u64) -> Vec<(u64, String)> {
    store
        .conversation_events(session_id, after, 100)
        .unwrap()
        .into_iter()
        .map(|record| (record.position, record.event.id))
        .collect()
}

#[test]
fn diagnostics_are_excluded_and_cursor_is_stable_across_reopen() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("state.db");
    let mut store = Store::open(&path).unwrap();
    let session = create(&mut store);
    let fence = store.begin_attempt(&session.id, session.version).unwrap();
    let diagnostics = [
        (
            "ready",
            "adapter_ready",
            json!({"type":"ready","version":1}),
        ),
        ("tool", "tool_request", json!({"type":"tool_call"})),
        ("failed", "adapter_failed", json!({"type":"failed"})),
        ("snapshot", "workspace_snapshot", json!({"status":"failed"})),
        ("custom", "custom_diagnostic", json!({"text":"hidden"})),
    ];
    for (id, kind, data) in diagnostics {
        store.append_event(&fence, id, 0, kind, data).unwrap();
    }
    store
        .append_event(&fence, "reply", 0, "text", text(&fence, "hello"))
        .unwrap();
    let settled = json!({"type":"settled","attempt_id":fence.attempt_id,
        "outcome":"success","result":"done"});
    store
        .append_event(&fence, "settled", 0, "adapter_settled", settled.clone())
        .unwrap();
    store
        .finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Success), None)
        .unwrap();
    let all = store.conversation_events(&session.id, 0, 100).unwrap();
    assert_eq!(
        kinds(&store, &session.id, 0),
        vec![(6, "reply".into()), (7, "settled".into())]
    );
    assert_eq!(all[1].event.data, settled);
    assert!(all.iter().all(|record| record.message.is_none()));
    let first = store.conversation_events(&session.id, 0, 1).unwrap();
    assert_eq!(first.len(), 1);
    drop(store);
    let store = Store::open(&path).unwrap();
    let rest = store
        .conversation_events(&session.id, first[0].position, 10)
        .unwrap();
    assert_eq!(rest, all[1..].to_vec());
    assert!(
        store
            .conversation_events(&session.id, 7, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .conversation_events(&session.id, 0, 0)
            .unwrap()
            .is_empty()
    );
    assert!(store.conversation_events(&session.id, u64::MAX, 1).is_err());
    assert!(store.conversation_events("missing", 0, 1).is_err());
}

#[test]
fn highest_revision_replaces_content_at_first_position() {
    let mut store = Store::open(":memory:").unwrap();
    let session = create(&mut store);
    let fence = store.begin_attempt(&session.id, session.version).unwrap();
    store
        .append_event(&fence, "a", 0, "text", text(&fence, "a0"))
        .unwrap();
    store
        .append_event(&fence, "b", 0, "text", text(&fence, "b0"))
        .unwrap();
    store
        .append_event(&fence, "a", 2, "text", text(&fence, "a2"))
        .unwrap();
    store
        .append_event(&fence, "a", 5, "text", text(&fence, "a5"))
        .unwrap();
    let records = store.conversation_events(&session.id, 0, 10).unwrap();
    let projected: Vec<_> = records
        .iter()
        .map(|r| {
            (
                r.position,
                r.event.sequence,
                r.event.revision,
                r.event.data["text"].clone(),
            )
        })
        .collect();
    assert_eq!(
        projected,
        vec![(1, 4, 5, json!("a5")), (2, 2, 0, json!("b0"))]
    );
    assert_eq!(store.events(&session.id, 0, 10).unwrap().len(), 4);
    assert_eq!(kinds(&store, &session.id, 1), vec![(2, "b".into())]);
}

#[test]
fn only_durably_delivered_follow_ups_are_included() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("state.db");
    let mut store = Store::open(&path).unwrap();
    let session = create(&mut store);
    store.enqueue_message(&session.id, "one", "first").unwrap();
    store.enqueue_message(&session.id, "two", "second").unwrap();
    store
        .enqueue_message(&session.id, "three", "third")
        .unwrap();
    store
        .enqueue_message(&session.id, "four", "fourth")
        .unwrap();
    store.withdraw_message(&session.id, "three").unwrap();
    let fence = store.begin_attempt(&session.id, session.version).unwrap();
    deliver(&mut store, &fence, "one");
    // A receipt recorded before acknowledgment is uncertain delivery, not conversation.
    assert_eq!(store.claim_next_message(&fence).unwrap().unwrap().id, "two");
    store
        .append_event(
            &fence,
            "message-accepted:two",
            0,
            "message_accepted",
            json!({"type":"message_accepted","attempt_id":fence.attempt_id,
                "message_id":"two","receipt":"r"}),
        )
        .unwrap();
    let forged = json!({"type":"message_accepted","message_id":"four"});
    store
        .append_event(&fence, "forged", 0, "message_accepted", forged)
        .unwrap();
    let records = store.conversation_events(&session.id, 0, 10).unwrap();
    assert_eq!(records.len(), 1);
    let message = records[0].message.as_ref().unwrap();
    assert_eq!(
        (message.id.as_str(), message.text.as_str()),
        ("one", "first")
    );
    assert_eq!(
        message.attempt_id.as_deref(),
        Some(fence.attempt_id.as_str())
    );
    store.acknowledge_message(&fence, "two").unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        kinds(&store, &session.id, 0),
        vec![
            (1, "message-accepted:one".into()),
            (2, "message-accepted:two".into())
        ]
    );
}
