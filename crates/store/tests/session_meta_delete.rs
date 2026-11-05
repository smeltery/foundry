//! Tombstone delete: refusal while running, dropped content, retained
//! witness, ledger and audit rows.
mod support_witness;
use foundry_contracts::{ContractError, CreateSession, Lifecycle, Outcome};
use foundry_store::{ArchiveFilter, SessionFilter, Store, StoreError, ToolOutcome};
use rusqlite::Connection;
use serde_json::json;
use support_witness::{checkpoint, input, terminal};
use tempfile::tempdir;

fn request(prompt: &str) -> CreateSession {
    CreateSession {
        prompt: prompt.into(),
        directory: "/workspace/project".into(),
        ..Default::default()
    }
}

fn tags(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
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

/// Every row of `table` for one session, rendered with SQLite's own quoting.
fn snapshot(db: &Connection, table: &str, session: &str) -> Vec<String> {
    let mut names = db
        .prepare(&format!(
            "SELECT name FROM pragma_table_info('{table}') ORDER BY cid"
        ))
        .unwrap();
    let names: Vec<String> = names
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let quoted = names
        .iter()
        .map(|name| format!("quote({name})"))
        .collect::<Vec<_>>()
        .join("||','||");
    let mut rows = db
        .prepare(&format!(
            "SELECT {quoted} FROM {table} WHERE session_id=?1 ORDER BY rowid"
        ))
        .unwrap();
    rows.query_map([session], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
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
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut store = Store::open(&path).unwrap();
    let keyed = CreateSession {
        idempotency_key: Some("delete-key".into()),
        tags: tags(&["alpha"]),
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

    let witnessed = checkpoint::session(&mut store);
    let fence = store.begin_attempt(&witnessed, 0).unwrap();
    let value = input(&store, &fence, "witness");
    let published = store
        .finish_with_witnessed_checkpoint(&fence, value, terminal())
        .unwrap();
    let witness = store
        .read_checkpoint_witness(&witnessed, &published.checkpoint.id)
        .unwrap();

    let db = Connection::open(&path).unwrap();
    let kept = [
        "attempts",
        "tool_calls",
        "context_checkpoints",
        "context_checkpoint_heads",
        "context_witnesses",
        "completion_decisions",
    ];
    let ledger = |session: &str| -> Vec<Vec<String>> {
        kept.iter()
            .map(|table| snapshot(&db, table, session))
            .collect()
    };
    let lineage = |session: &str| -> String {
        db.query_row(
            "SELECT quote(id)||quote(idempotency_key)||quote(origin_kind)||quote(parent_session_id)
                ||quote(forked_from_session_id)||quote(json_extract(body,'$.lifecycle'))
                ||quote(json_extract(body,'$.outcome'))||quote(json_extract(body,'$.version'))
             FROM sessions WHERE id=?1",
            [session],
            |r| r.get(0),
        )
        .unwrap()
    };
    let before = [ledger(&tooled), ledger(&witnessed)];
    let lineage_before = [lineage(&tooled), lineage(&witnessed)];
    assert!(before[0][1].len() == 1 && before[1][4].len() == 1 && before[1][5].len() == 1);

    for id in [&tooled, &witnessed] {
        let receipt = store.delete_session(id).unwrap();
        assert_eq!(&receipt.session_id, id);
        not_found(store.get(id));
        not_found(store.events(id, 0, 10));
        not_found(store.messages(id, 0, 10));
        not_found(store.session_tags(id));
        not_found(store.associations(id));
        not_found(store.set_archived(id, true));
        not_found(store.rename_session(id, Some("again")));
        not_found(store.enqueue_message(id, "m2", "more"));
        not_found(store.begin_attempt(id, 99));
        not_found(store.delete_session(id));
    }
    assert!(store.list(10).unwrap().is_empty());
    assert!(listed(&store, ArchiveFilter::All).is_empty());
    // The kept key cannot create a new session over the tombstone.
    conflict(store.create_session(keyed));

    assert_eq!([ledger(&tooled), ledger(&witnessed)], before);
    assert_eq!([lineage(&tooled), lineage(&witnessed)], lineage_before);
    // The witness still verifies against its stored checkpoint after delete.
    let reread = store
        .read_checkpoint_witness(&witnessed, &published.checkpoint.id)
        .unwrap();
    assert_eq!(reread.digest(), witness.digest());

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
    let body: String = db
        .query_row("SELECT body FROM sessions WHERE id=?1", [&tooled], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(
        !body.contains("private") && !body.contains("Visible"),
        "{body}"
    );
    let leftovers: i64 = db
        .query_row(
            "SELECT (SELECT count(*) FROM messages WHERE text!='')
                + (SELECT count(*) FROM events WHERE data!='null')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leftovers, 0);
    drop(store);
    // A reopened store keeps the tombstones.
    let store = Store::open(&path).unwrap();
    not_found(store.get(&tooled));
}
