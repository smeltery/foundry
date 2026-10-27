use foundry_contracts::{CreateSession, Event, Message, Session};
use foundry_store::{AttemptFence, Result, Store, ToolOutcome, ToolRecord};
use rusqlite::Connection;
use serde_json::json;
use std::sync::{Arc, Barrier};
use tempfile::tempdir;

#[derive(Clone, Copy, Debug)]
enum Mutation {
    Begin,
    Event,
    MessageClaim,
    MessageAck,
    ToolClaim,
    ToolFinish,
}
const MUTATIONS: [Mutation; 6] = [
    Mutation::Begin,
    Mutation::Event,
    Mutation::MessageClaim,
    Mutation::MessageAck,
    Mutation::ToolClaim,
    Mutation::ToolFinish,
];

fn create(store: &mut Store) -> Session {
    store
        .create(CreateSession {
            prompt: "atomic revision".into(),
            directory: "/tmp".into(),
            model: None,
            idempotency_key: None,
        })
        .unwrap()
}
fn prepare(store: &mut Store, mutation: Mutation) -> (Session, Option<AttemptFence>) {
    let session = create(store);
    if matches!(mutation, Mutation::Begin) {
        return (session, None);
    }
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    if matches!(mutation, Mutation::MessageClaim | Mutation::MessageAck) {
        store
            .enqueue_message(&session.id, "message", "instruction")
            .unwrap();
    }
    if matches!(mutation, Mutation::MessageAck) {
        store.claim_next_message(&fence).unwrap();
    }
    if matches!(mutation, Mutation::ToolFinish) {
        store.claim_tool(&fence, "call", "read", [1; 32]).unwrap();
    }
    (session, Some(fence))
}
fn apply(
    store: &mut Store,
    session: &Session,
    fence: &Option<AttemptFence>,
    mutation: Mutation,
) -> Result<()> {
    if matches!(mutation, Mutation::Begin) {
        return store.begin_attempt(&session.id, 0).map(|_| ());
    }
    let fence = fence.as_ref().unwrap();
    match mutation {
        Mutation::Event => store
            .append_event(fence, "event", 0, "text", json!("value"))
            .map(|_| ()),
        Mutation::MessageClaim => store.claim_next_message(fence).map(|_| ()),
        Mutation::MessageAck => store.acknowledge_message(fence, "message").map(|_| ()),
        Mutation::ToolClaim => store.claim_tool(fence, "call", "read", [1; 32]).map(|_| ()),
        Mutation::ToolFinish => store
            .finish_tool(fence, "call", ToolOutcome::Success, Some([2; 32]))
            .map(|_| ()),
        Mutation::Begin => unreachable!(),
    }
}
#[derive(Debug, PartialEq)]
struct Snapshot {
    session: Session,
    revision: u64,
    events: Vec<Event>,
    messages: Vec<Message>,
    tool: Option<ToolRecord>,
    attempts: i64,
}
fn snapshot(store: &Store, sql: &Connection, id: &str) -> Snapshot {
    Snapshot {
        session: store.get(id).unwrap(),
        revision: store.work_revision(id).unwrap(),
        events: store.events(id, 0, 100).unwrap(),
        messages: store.messages(id, 0, 100).unwrap(),
        tool: store.tool_call(id, "call").unwrap(),
        attempts: sql
            .query_row(
                "SELECT count(*) FROM attempts WHERE session_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap(),
    }
}

#[test]
fn overflow_rejects_every_mutation_without_partial_state() {
    for mutation in MUTATIONS {
        let directory = tempdir().unwrap();
        let path = directory.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let (session, fence) = prepare(&mut store, mutation);
        let sql = Connection::open(&path).unwrap();
        sql.execute(
            "UPDATE session_work SET revision=?1 WHERE session_id=?2",
            rusqlite::params![i64::MAX, session.id],
        )
        .unwrap();
        let before = snapshot(&store, &sql, &session.id);
        assert!(
            apply(&mut store, &session, &fence, mutation).is_err(),
            "{mutation:?}"
        );
        assert_eq!(snapshot(&store, &sql, &session.id), before, "{mutation:?}");
    }
}

#[test]
fn later_mutation_failure_rolls_back_the_revision_increment() {
    for mutation in MUTATIONS {
        let directory = tempdir().unwrap();
        let path = directory.path().join("db");
        let mut store = Store::open(&path).unwrap();
        let (session, fence) = prepare(&mut store, mutation);
        let sql = Connection::open(&path).unwrap();
        let operation = match mutation {
            Mutation::Begin => "INSERT ON attempts",
            Mutation::Event => "INSERT ON events",
            Mutation::MessageClaim | Mutation::MessageAck => "UPDATE ON messages",
            Mutation::ToolClaim => "INSERT ON tool_calls",
            Mutation::ToolFinish => "UPDATE ON tool_calls",
        };
        sql.execute_batch(&format!("CREATE TRIGGER reject_mutation BEFORE {operation} BEGIN SELECT RAISE(FAIL,'injected failure'); END;")).unwrap();
        let before = snapshot(&store, &sql, &session.id);
        assert!(
            apply(&mut store, &session, &fence, mutation).is_err(),
            "{mutation:?}"
        );
        assert_eq!(snapshot(&store, &sql, &session.id), before, "{mutation:?}");
    }
}

#[test]
fn maximum_revision_still_allows_exact_retries_and_nonwork_operations() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = create(&mut store);
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    let event = store
        .append_event(&fence, "event", 0, "text", json!("value"))
        .unwrap();
    store
        .enqueue_message(&session.id, "message", "input")
        .unwrap();
    store.claim_next_message(&fence).unwrap();
    let message = store.acknowledge_message(&fence, "message").unwrap();
    store.claim_tool(&fence, "call", "read", [1; 32]).unwrap();
    let receipt = store
        .finish_tool(&fence, "call", ToolOutcome::Success, Some([2; 32]))
        .unwrap();
    let sql = Connection::open(&path).unwrap();
    sql.execute("UPDATE session_work SET revision=?1", [i64::MAX])
        .unwrap();
    assert_eq!(
        store
            .append_event(&fence, "event", 0, "text", json!("value"))
            .unwrap(),
        event
    );
    assert_eq!(
        store.acknowledge_message(&fence, "message").unwrap(),
        message
    );
    assert_eq!(
        store
            .finish_tool(&fence, "call", ToolOutcome::Success, Some([2; 32]))
            .unwrap(),
        receipt
    );
    store.heartbeat(&fence).unwrap();
    store
        .enqueue_message(&session.id, "pending", "later")
        .unwrap();
    store.withdraw_message(&session.id, "pending").unwrap();
    store
        .request_cancel(&session.id, foundry_contracts::CancelMode::Hard)
        .unwrap();
    assert_eq!(store.work_revision(&session.id).unwrap(), i64::MAX as u64);
}

#[test]
fn final_signed_integer_increment_succeeds_without_real_promotion() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = create(&mut store);
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    let sql = Connection::open(&path).unwrap();
    sql.execute("UPDATE session_work SET revision=?1", [i64::MAX - 1])
        .unwrap();
    store
        .append_event(&fence, "last", 0, "text", json!(null))
        .unwrap();
    assert_eq!(store.work_revision(&session.id).unwrap(), i64::MAX as u64);
    assert_eq!(
        sql.query_row("SELECT typeof(revision) FROM session_work", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "integer"
    );
    assert!(
        store
            .append_event(&fence, "overflow", 0, "text", json!(null))
            .is_err()
    );
    for invalid in ["-1", "1.5", "'invalid'", "9223372036854775808"] {
        assert!(
            sql.execute_batch(&format!("UPDATE session_work SET revision={invalid}"))
                .is_err()
        );
    }
    assert_eq!(store.events(&session.id, 0, 100).unwrap().len(), 1);
    assert_eq!(store.work_revision(&session.id).unwrap(), i64::MAX as u64);
}

#[test]
fn missing_revision_row_is_not_silently_reinitialized() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = create(&mut store);
    let sql = Connection::open(&path).unwrap();
    sql.execute("DELETE FROM session_work", []).unwrap();
    assert!(store.work_revision(&session.id).is_err());
    assert!(store.begin_attempt(&session.id, 0).is_err());
    assert_eq!(store.get(&session.id).unwrap(), session);
    assert_eq!(
        sql.query_row("SELECT count(*) FROM attempts", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn concurrent_connections_do_not_lose_revision_increments() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let session = create(&mut store);
    let fence = store.begin_attempt(&session.id, 0).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for owner in 0..2 {
        let path = path.clone();
        let fence = fence.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            let mut store = Store::open(path).unwrap();
            barrier.wait();
            for index in 0..16 {
                store
                    .append_event(&fence, &format!("{owner}-{index}"), 0, "text", json!(index))
                    .unwrap();
            }
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(store.work_revision(&session.id).unwrap(), 33);
    assert_eq!(store.events(&session.id, 0, 100).unwrap().len(), 32);
}
