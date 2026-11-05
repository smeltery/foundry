//! Tombstone delete: refusal while running, dropped content, kept witness,
//! ledger, continuation and audit rows, and not_found from every per-session read.
mod support_acceptance;
use foundry_contracts::{ContractError, CreateSession, Lifecycle, Outcome};
use foundry_store::{ArchiveFilter, SessionFilter, Store, StoreError, ToolOutcome};
use rusqlite::Connection;
use serde_json::json;
use support_acceptance::{
    Fixture,
    witness::{checkpoint, input, terminal},
};

fn request(prompt: &str) -> CreateSession {
    CreateSession {
        prompt: prompt.into(),
        directory: "/workspace/project".into(),
        ..Default::default()
    }
}

fn listed(store: &Store, archive: ArchiveFilter) -> Vec<String> {
    let filter = SessionFilter {
        archive,
        ..Default::default()
    };
    let page = store.query_sessions(&filter, None, 200).unwrap();
    page.sessions
        .into_iter()
        .map(|session| session.id)
        .collect()
}

fn not_found<T>(result: foundry_store::Result<T>) {
    match result {
        Err(StoreError::Contract(ContractError::NotFound(_))) => {}
        Err(other) => panic!("expected not_found, got {other}"),
        Ok(_) => panic!("expected not_found, got success"),
    }
}

fn conflict<T>(result: foundry_store::Result<T>) {
    match result {
        Err(StoreError::Contract(ContractError::Conflict(_))) => {}
        Err(other) => panic!("expected conflict, got {other}"),
        Ok(_) => panic!("expected conflict, got success"),
    }
}

/// Kept tables and how each row is linked to its session.
const KEPT: [(&str, &str); 11] = [
    ("attempts", "session_id=?1"),
    ("tool_calls", "session_id=?1"),
    ("context_checkpoints", "session_id=?1"),
    ("context_checkpoint_heads", "session_id=?1"),
    ("context_witnesses", "session_id=?1"),
    ("completion_decisions", "session_id=?1"),
    ("context_restores", "session_id=?1"),
    ("context_continue_messages", "session_id=?1"),
    ("message_reconciliations", "session_id=?1"),
    (
        "context_continue_acceptances",
        "restore_id IN (SELECT id FROM context_restores WHERE session_id=?1)",
    ),
    (
        "context_finalizations",
        "checkpoint_id IN (SELECT id FROM context_checkpoints WHERE session_id=?1)",
    ),
];

/// Every kept row of one session, rendered with SQLite's own quoting.
fn kept_rows(db: &Connection, session: &str) -> Vec<(&'static str, Vec<String>)> {
    KEPT.iter()
        .map(|&(table, linked)| {
            let mut names = db
                .prepare(&format!(
                    "SELECT name FROM pragma_table_info('{table}') ORDER BY cid"
                ))
                .unwrap();
            let quoted = names
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .map(|name| format!("quote({})", name.unwrap()))
                .collect::<Vec<_>>()
                .join("||','||");
            let mut rows = db
                .prepare(&format!(
                    "SELECT {quoted} FROM {table} WHERE {linked} ORDER BY rowid"
                ))
                .unwrap();
            let rows = rows
                .query_map([session], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            (table, rows)
        })
        .collect()
}

#[test]
fn delete_refuses_a_live_attempt_or_restore_owner() {
    let mut store = Store::open(":memory:").unwrap();
    let id = store.create(request("running sample")).unwrap().id;
    let fence = store.begin_attempt(&id, 0).unwrap();
    conflict(store.delete_session(&id));
    assert_eq!(store.get(&id).unwrap().lifecycle, Lifecycle::Running);
    store.heartbeat(&fence).unwrap();
    store
        .finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Failure), None)
        .unwrap();

    let session = checkpoint::session(&mut store);
    let fence = store.begin_attempt(&session, 0).unwrap();
    let value = input(&store, &fence, "witness");
    let published = store
        .finish_with_witnessed_checkpoint(&fence, value, terminal())
        .unwrap();
    let restore = foundry_store::BeginRestoreInput {
        request_id: "load".into(),
        checkpoint_id: published.checkpoint.id.clone(),
        expected_session_version: store.get(&session).unwrap().version,
        configuration_digest: [7; 32],
    };
    store.begin_restore(&session, restore).unwrap();
    conflict(store.delete_session(&session));
    assert!(store.get(&session).is_ok());
    store.delete_session(&id).unwrap();
}

#[test]
fn delete_tombstones_content_and_keeps_witness_ledger_and_audit_rows() {
    // A witnessed checkpoint, restore, accepted continuation and settled attempt.
    let mut f = Fixture::new();
    let continued = f.fence.session_id.clone();
    let baseline = f.input.intent.baseline_checkpoint_id.clone();
    f.accept().unwrap();
    assert!(f.lookup().unwrap().is_some());
    f.store
        .finish_attempt(&f.fence, Lifecycle::Exit, Some(Outcome::Failure), None)
        .unwrap();
    let db = f.db();
    db.execute(
        "INSERT INTO message_reconciliations(session_id,message_id,ordinal,attempt_id,prior_state,
            resolved_state,cause,receipt_digest,recorded_ms)
         VALUES(?1,'instruction',1,?2,'in_flight','delivered','operator_confirmed',zeroblob(32),1)",
        [&continued, &f.fence.attempt_id],
    )
    .unwrap();

    let store = &mut f.store;
    let keyed = CreateSession {
        idempotency_key: Some("delete-key".into()),
        tags: vec!["alpha".into()],
        title: Some("Visible title".into()),
        ..request("tombstone sample")
    };
    let tooled = store.create(keyed.clone()).unwrap().id;
    let fence = store.begin_attempt(&tooled, 0).unwrap();
    store
        .append_event(&fence, "e1", 0, "text", json!("private event"))
        .unwrap();
    store
        .enqueue_message(&tooled, "m1", "private follow-up")
        .unwrap();
    store.claim_tool(&fence, "call", "write", [1; 32]).unwrap();
    store
        .finish_tool(&fence, "call", ToolOutcome::Success, Some([3; 32]))
        .unwrap();
    store
        .finish_attempt(
            &fence,
            Lifecycle::Exit,
            Some(Outcome::Failure),
            Some("private result".into()),
        )
        .unwrap();
    let queued = store.create(request("queued private prompt")).unwrap().id;
    store.enqueue_message(&queued, "q1", "queued text").unwrap();

    let lineage = |session: &str| -> String {
        db.query_row(
            "SELECT quote(id)||quote(idempotency_key)||quote(origin_kind)||quote(parent_session_id)
                ||quote(forked_from_session_id)||quote(json_extract(body,'$.lifecycle'))
                ||quote(json_extract(body,'$.outcome'))||quote(json_extract(body,'$.version'))
                ||quote(json_extract(body,'$.directory'))||quote(json_extract(body,'$.model'))
             FROM sessions WHERE id=?1",
            [session],
            |r| r.get(0),
        )
        .unwrap()
    };
    let sessions = [&tooled, &continued, &queued];
    let before: Vec<_> = sessions.iter().map(|id| kept_rows(&db, id)).collect();
    let lineage_before: Vec<_> = sessions.iter().map(|id| lineage(id)).collect();
    let count = |id: &str, table: &str| {
        let rows = kept_rows(&db, id);
        rows.iter()
            .find(|(name, _)| *name == table)
            .unwrap()
            .1
            .len()
    };
    assert_eq!(count(&tooled, "tool_calls"), 1);
    for table in [
        "context_witnesses",
        "context_restores",
        "context_continue_messages",
        "context_continue_acceptances",
        "context_finalizations",
        "message_reconciliations",
    ] {
        assert!(count(&continued, table) >= 1, "{table}");
    }
    assert_eq!(count(&continued, "completion_decisions"), 2);

    for id in sessions {
        let receipt = store.delete_session(id).unwrap();
        assert_eq!(&receipt.session_id, id);
        not_found(store.get(id));
        not_found(store.events(id, 0, 10));
        not_found(store.messages(id, 0, 10));
        not_found(store.withdraw_message(id, "q1"));
        not_found(store.session_tags(id));
        not_found(store.associations(id));
        not_found(store.session_completion(id));
        not_found(store.completion_decision(id, &fence.attempt_id));
        not_found(store.current_completion_decision(id));
        not_found(store.checkpoint_head(id));
        not_found(store.set_archived(id, true));
        not_found(store.rename_session(id, Some("again")));
        not_found(store.enqueue_message(id, "m2", "more"));
        not_found(store.begin_attempt(id, 99));
        not_found(store.begin_fresh_attempt(id, 0));
        not_found(store.delete_session(id));
    }
    not_found(store.read_checkpoint_witness(&continued, &baseline));
    not_found(store.continuation_acceptance(
        &continued,
        &f.input.intent.restore_id,
        f.input.request_digest().unwrap(),
    ));
    let store = &mut f.store;
    assert!(store.list(10).unwrap().is_empty());
    assert!(listed(store, ArchiveFilter::All).is_empty());
    // The kept key cannot create a new session over the tombstone.
    conflict(store.create_session(keyed));

    // Digest and evidence rows are unchanged, though their message and event
    // bytes are gone, so they can no longer be re-checked against content.
    let after: Vec<_> = sessions.iter().map(|id| kept_rows(&db, id)).collect();
    assert_eq!(after, before);
    let lineage_after: Vec<_> = sessions.iter().map(|id| lineage(id)).collect();
    assert_eq!(lineage_after, lineage_before);

    let content: (String, String, Option<String>, i64) = db
        .query_row(
            "SELECT json_extract(body,'$.prompt'),creation,title,
                (SELECT count(*) FROM session_tags WHERE session_id=?1)
             FROM sessions WHERE id=?1",
            [&tooled],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(content, (String::new(), "{}".into(), None, 0));
    let bodies: Vec<String> = db
        .prepare("SELECT body FROM sessions")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for body in &bodies {
        assert!(
            !body.contains("private") && !body.contains("Visible"),
            "{body}"
        );
        assert!(body.contains("\"directory\":"), "{body}");
    }
    let leftovers: i64 = db
        .query_row(
            "SELECT (SELECT count(*) FROM messages WHERE text!='')
                + (SELECT count(*) FROM events WHERE data!='null')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leftovers, 0);
    // A reopened store keeps the tombstones.
    let store = Store::open(f.dir.path().join("db")).unwrap();
    not_found(store.get(&tooled));
}
