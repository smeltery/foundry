use foundry_contracts::{ContractError, CreateSession, ModelConfig};
use foundry_store::{AttemptFence, Store, StoreError};
use rusqlite::Connection;
use std::path::Path;
use tempfile::tempdir;

// Written by the v11 store through its public API; see the header of the fixture.
const V11: &str = include_str!("fixtures/store-v11.sql");
const RUNNING: &str = "54b004cc-b044-4cfd-9a4b-7c88a3ab7bdc";
const QUEUED: &str = "b7213685-ed29-4eaa-bf8e-c79f17c7b050";

fn fixture(path: &Path) -> Connection {
    let db = Connection::open(path).unwrap();
    db.execute_batch(V11).unwrap();
    db
}

fn version(db: &Connection) -> i64 {
    db.pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

fn columns(db: &Connection, table: &str) -> Vec<String> {
    let mut statement = db
        .prepare(&format!(
            "SELECT name FROM pragma_table_info('{table}') ORDER BY cid"
        ))
        .unwrap();
    statement
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Every row of `table`, restricted to `names`, rendered with SQLite's own quoting.
fn rows(db: &Connection, table: &str, names: &[String]) -> Vec<String> {
    let quoted = names
        .iter()
        .map(|name| format!("quote({name})"))
        .collect::<Vec<_>>()
        .join("||','||");
    let mut statement = db
        .prepare(&format!("SELECT {quoted} FROM {table} ORDER BY rowid"))
        .unwrap();
    statement
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn hex(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn v11_fixture_migrates_with_rows_sessions_and_cut_hashes_unchanged() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store.db");
    let db = fixture(&path);
    assert_eq!(version(&db), 11);
    let tables = rows(&db, "sqlite_master WHERE type='table'", &["name".into()]);
    assert_eq!(tables.len(), 13);
    let before: Vec<_> = tables
        .iter()
        .map(|quoted| {
            let table = quoted.trim_matches('\'').to_owned();
            let names = columns(&db, &table);
            let snapshot = rows(&db, &table, &names);
            (table, names, snapshot)
        })
        .collect();
    let bodies = rows(&db, "sessions", &["body".into()]);
    assert_eq!(bodies.len(), 3);

    let store = Store::open(&path).unwrap();
    assert_eq!(version(&db), 12);
    for (table, names, rows_before) in &before {
        let after = columns(&db, table);
        assert_eq!(&after[..names.len()], &names[..], "{table} column order");
        assert_eq!(&rows(&db, table, names), rows_before, "{table} rows");
    }
    assert_eq!(
        columns(&db, "sessions")[4..],
        [
            "archived",
            "title",
            "origin_kind",
            "origin_interface",
            "created_ms",
            "updated_ms",
            "parent_session_id",
            "forked_from_session_id"
        ]
    );
    assert_eq!(columns(&db, "events")[7..], ["attempt_ordinal", "position"]);
    let legacy: (i64, i64) = db
        .query_row(
            "SELECT (SELECT count(*) FROM sessions WHERE archived=0 AND title IS NULL AND origin_kind IS NULL
                AND created_ms IS NULL AND parent_session_id IS NULL AND forked_from_session_id IS NULL),
             (SELECT count(*) FROM events WHERE attempt_ordinal IS NULL AND position IS NULL)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(legacy, (3, 4));

    // Sessions decode with empty metadata and re-encode to the stored v11 bytes.
    for session in store.list(10).unwrap() {
        assert_eq!(session.metadata, Default::default());
        let encoded = serde_json::to_string(&session).unwrap();
        assert!(bodies.contains(&format!("'{encoded}'")), "{encoded}");
        assert_eq!(store.get(&session.id).unwrap(), session);
    }

    // The fixture's attempt lease runs for a century, so the v11 fence is still valid.
    let fence = AttemptFence {
        session_id: RUNNING.into(),
        attempt_id: "648b25b9-f347-4f21-afbf-82b37c4e889a".into(),
        token: "b4210a18-df5e-4df4-babe-9f72e7a22573".into(),
    };
    let cut = store.checkpoint_cut(&fence).unwrap();
    // Values printed by the v11 store when it wrote the fixture.
    assert_eq!(cut.work_revision, 8);
    assert_eq!(cut.event_seq, 3);
    assert_eq!(
        hex(cut.event_digest),
        "48288d7c92a250aeb1892b07de37f9f4e150ea4c584d45de6a91c6d0e70f797d"
    );
    assert_eq!(
        hex(cut.input_digest),
        "af14c7c8322599de79fd9671eaf845bb7b1f17472e08594e38f4b60ef573eee4"
    );
    assert_eq!(
        hex(cut.tool_digest),
        "656eb918c060f90f7f58f5fee17fa755b5bb8c8986b937007c30e848ad4840b7"
    );
    assert_eq!(store.capture_snapshot(&fence).unwrap().cut, cut);
    drop(store);
    drop(Store::open(&path).unwrap());
    assert_eq!(version(&db), 12);
}

#[test]
fn old_format_idempotent_create_replays_the_original_session_after_migration() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store.db");
    let db = fixture(&path);
    let mut store = Store::open(&path).unwrap();
    let request = CreateSession {
        prompt: "Summarize the sample notes.\nThen list open items.".into(),
        directory: "/workspace/sample".into(),
        model: Some(ModelConfig {
            provider: "local".into(),
            model: "deterministic".into(),
            effort: Some("low".into()),
        }),
        idempotency_key: Some("fixture-key-1".into()),
        ..Default::default()
    };
    // The comparison is byte equality against what the v11 store recorded.
    let recorded: String = db
        .query_row(
            "SELECT creation FROM sessions WHERE id=?1",
            [RUNNING],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(serde_json::to_string(&request).unwrap(), recorded);
    let original = store.get(RUNNING).unwrap();
    assert_eq!(store.create(request.clone()).unwrap(), original);
    let queued = CreateSession {
        prompt: "Draft a plan.".into(),
        directory: "/workspace/sample".into(),
        idempotency_key: Some("fixture-key-2".into()),
        ..Default::default()
    };
    assert_eq!(store.create(queued.clone()).unwrap().id, QUEUED);

    let changed = CreateSession {
        prompt: "Draft another plan.".into(),
        ..queued.clone()
    };
    assert!(matches!(
        store.create(changed),
        Err(StoreError::Contract(ContractError::Conflict(_)))
    ));
    // A v2 option is never silently dropped: it is unsupported until its owner stores it.
    for v2 in [
        CreateSession {
            title: Some("Sample".into()),
            ..queued.clone()
        },
        CreateSession {
            tags: vec!["alpha".into()],
            ..request
        },
    ] {
        assert!(matches!(
            store.create(v2),
            Err(StoreError::Contract(ContractError::Unsupported(_)))
        ));
    }
    assert_eq!(store.list(10).unwrap().len(), 3);
}

#[test]
fn v12_tables_enforce_their_bounds() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store.db");
    let db = fixture(&path);
    drop(Store::open(&path).unwrap());
    db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let attempt = "648b25b9-f347-4f21-afbf-82b37c4e889a";
    let decision = |policy: &str, basis: &str, digest: usize| {
        db.execute(
            "INSERT INTO completion_decisions(attempt_id,session_id,policy_version,configuration_digest,
                criteria_digest,work_revision,event_sequence,checkpoint_id,outcome,basis,cause,
                criterion_results,committed_ms)
             VALUES(?1,?2,?3,zeroblob(32),zeroblob(?4),8,3,NULL,'action_required',?5,'criteria_unverified','[]',1)",
            rusqlite::params![attempt, RUNNING, policy, digest, basis],
        )
    };
    assert!(decision("foundry.completion.v2", "default", 32).is_err());
    assert!(decision("foundry.completion.v1", "model", 32).is_err());
    assert!(decision("foundry.completion.v1", "default", 31).is_err());
    decision("foundry.completion.v1", "default", 32).unwrap();
    assert!(decision("foundry.completion.v1", "default", 32).is_err());

    let tag = |session: &str, tag: &str, position: i64| {
        db.execute(
            "INSERT INTO session_tags(session_id,tag,position) VALUES(?1,?2,?3)",
            rusqlite::params![session, tag, position],
        )
    };
    tag(RUNNING, "alpha", 0).unwrap();
    assert!(tag(RUNNING, "alpha", 1).is_err());
    assert!(tag(RUNNING, "beta", 0).is_err());
    assert!(tag(RUNNING, "", 1).is_err());
    assert!(tag("missing", "beta", 1).is_err());

    let audit = |message: &str, resolved: &str| {
        db.execute(
            "INSERT INTO message_reconciliations(session_id,message_id,ordinal,attempt_id,prior_state,
                resolved_state,cause,receipt_digest,recorded_ms)
             VALUES(?1,?2,1,?3,'in_flight',?4,'operator_confirmed',NULL,1)",
            rusqlite::params![RUNNING, message, attempt, resolved],
        )
    };
    assert!(audit("m3", "in_flight").is_err());
    assert!(audit("absent", "delivered").is_err());
    audit("m3", "pending").unwrap();

    for update in [
        "UPDATE sessions SET archived=2",
        "UPDATE sessions SET title=''",
        "UPDATE sessions SET origin_kind='robot'",
        "UPDATE sessions SET origin_interface='mcp'",
        "UPDATE sessions SET created_ms=-1",
        "UPDATE sessions SET parent_session_id='short'",
        "UPDATE events SET attempt_ordinal=0",
        "UPDATE events SET position=0",
    ] {
        assert!(db.execute_batch(update).is_err(), "{update}");
    }
    db.execute_batch(
        "UPDATE sessions SET origin_kind='external', origin_interface='mcp', archived=1;
         UPDATE events SET attempt_ordinal=1, position=sequence;",
    )
    .unwrap();
}
