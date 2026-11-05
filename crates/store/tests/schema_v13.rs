//! Schema v13 (tombstone delete) upgrades a populated v12 store additively.
use foundry_contracts::{CreateSession, Lifecycle, Outcome};
use foundry_store::Store;
use rusqlite::Connection;
use serde_json::json;
use tempfile::tempdir;

fn version(db: &Connection) -> i64 {
    db.pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

/// Every table's rows rendered with SQLite's own quoting, by table name.
fn dump(db: &Connection) -> Vec<(String, Vec<String>)> {
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    tables
        .into_iter()
        .map(|table| {
            let names: Vec<String> = db
                .prepare(&format!(
                    "SELECT name FROM pragma_table_info('{table}') WHERE name!='deleted_ms' ORDER BY cid"
                ))
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            let quoted = names
                .iter()
                .map(|name| format!("quote({name})"))
                .collect::<Vec<_>>()
                .join("||','||");
            let rows = db
                .prepare(&format!("SELECT {quoted} FROM {table} ORDER BY rowid"))
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            (table, rows)
        })
        .collect()
}

#[test]
fn populated_v12_store_upgrades_to_v13_with_rows_unchanged() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let settled = store
        .create(CreateSession {
            prompt: "settled".into(),
            directory: "/workspace/project".into(),
            tags: vec!["alpha".into()],
            title: Some("Settled".into()),
            idempotency_key: Some("key".into()),
            ..Default::default()
        })
        .unwrap();
    let fence = store.begin_attempt(&settled.id, 0).unwrap();
    store
        .append_event(&fence, "e1", 0, "text", json!("hello"))
        .unwrap();
    store
        .enqueue_message(&settled.id, "m1", "follow-up")
        .unwrap();
    store
        .finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Failure), None)
        .unwrap();
    let queued = store
        .create(CreateSession {
            prompt: "queued".into(),
            directory: "/workspace/project".into(),
            ..Default::default()
        })
        .unwrap();
    drop(store);

    // v13 only adds `sessions.deleted_ms`, so dropping it rebuilds the v12 schema.
    let db = Connection::open(&path).unwrap();
    db.execute_batch("ALTER TABLE sessions DROP COLUMN deleted_ms; PRAGMA user_version=12;")
        .unwrap();
    let before = dump(&db);
    assert!(
        before
            .iter()
            .any(|(table, rows)| table == "sessions" && rows.len() == 2)
    );

    let mut store = Store::open(&path).unwrap();
    assert_eq!(version(&db), 13);
    assert_eq!(dump(&db), before);
    let tombstones: i64 = db
        .query_row(
            "SELECT count(*) FROM sessions WHERE deleted_ms IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tombstones, 0);
    assert_eq!(store.get(&settled.id).unwrap().lifecycle, Lifecycle::Exit);
    assert_eq!(store.session_tags(&settled.id).unwrap(), ["alpha"]);
    assert_eq!(store.list(10).unwrap().len(), 2);

    // Migrated rows take the new operations.
    store.set_archived(&queued.id, true).unwrap();
    store.delete_session(&settled.id).unwrap();
    assert!(store.get(&settled.id).is_err());
    drop(store);
    drop(Store::open(&path).unwrap());
    assert_eq!(version(&db), 13);
}
