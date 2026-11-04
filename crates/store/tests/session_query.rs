use foundry_contracts::{
    CancelMode, ContractError, CreateSession, Session, SessionMetadata, SessionOrigin,
};
use foundry_store::{ArchiveFilter, OriginFilter, SessionFilter, Store, StoreError};
use rusqlite::{Connection, params};
use std::{path::Path, sync::mpsc, thread};
use tempfile::tempdir;

fn create(store: &mut Store, prompt: &str) -> Session {
    store
        .create(CreateSession {
            prompt: prompt.into(),
            directory: "/workspace/project".into(),
            ..Default::default()
        })
        .unwrap()
}

/// Writes v2 metadata the way a storing owner would: columns and body together.
/// `Store::create` still rejects these options, so tests set them directly.
fn set_metadata(path: &Path, id: &str, metadata: SessionMetadata) {
    let db = Connection::open(path).unwrap();
    let body: String = db
        .query_row("SELECT body FROM sessions WHERE id=?1", [id], |r| r.get(0))
        .unwrap();
    let mut session: Session = serde_json::from_str(&body).unwrap();
    let (kind, interface) = match &metadata.origin {
        None => (None, None),
        Some(SessionOrigin::Interactive {}) => (Some("interactive"), None),
        Some(SessionOrigin::External { interface }) => (Some("external"), interface.clone()),
    };
    db.execute(
        "UPDATE sessions SET archived=?1, title=?2, origin_kind=?3, origin_interface=?4 WHERE id=?5",
        params![metadata.archived, metadata.title, kind, interface, id],
    )
    .unwrap();
    *session.metadata = metadata;
    db.execute(
        "UPDATE sessions SET body=?1 WHERE id=?2",
        params![serde_json::to_string(&session).unwrap(), id],
    )
    .unwrap();
}

/// Every matching ID, following cursors with the largest page the filter allows.
fn ids(store: &Store, filter: &SessionFilter) -> Vec<String> {
    let size = if filter.title_search.is_some() {
        24
    } else {
        200
    };
    let mut found = Vec::new();
    let mut cursor = None;
    loop {
        let page = store
            .query_sessions(filter, cursor.as_deref(), size)
            .unwrap();
        found.extend(page.sessions.into_iter().map(|session| session.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            return found;
        }
    }
}

fn assert_invalid(result: Result<impl std::fmt::Debug, StoreError>) {
    match result {
        Err(StoreError::Contract(ContractError::Invalid(_))) => {}
        other => panic!("expected invalid_request, got {other:?}"),
    }
}

fn search(text: &str) -> SessionFilter {
    SessionFilter {
        title_search: Some(text.into()),
        ..Default::default()
    }
}

#[test]
fn newest_created_first_and_updates_do_not_reorder() {
    let mut store = Store::open(":memory:").unwrap();
    let first = create(&mut store, "first");
    let second = create(&mut store, "second");
    store
        .request_cancel(&first.id, CancelMode::Graceful)
        .unwrap();
    assert_eq!(
        ids(&store, &SessionFilter::default()),
        vec![second.id, first.id]
    );
}

#[test]
fn archive_filter_defaults_to_active() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let archived = create(&mut store, "archived");
    let active = create(&mut store, "active");
    let archived_metadata = SessionMetadata {
        archived: true,
        ..Default::default()
    };
    set_metadata(&path, &archived.id, archived_metadata);
    let filter = |archive| SessionFilter {
        archive,
        ..Default::default()
    };
    assert_eq!(
        ids(&store, &SessionFilter::default()),
        vec![active.id.clone()]
    );
    let archived_ids = ids(&store, &filter(ArchiveFilter::Archived));
    assert_eq!(archived_ids, vec![archived.id.clone()]);
    let every = ids(&store, &filter(ArchiveFilter::All));
    assert_eq!(every, vec![active.id, archived.id.clone()]);
    let stored = store.get(&archived.id).unwrap();
    assert!(stored.metadata.archived);
}

#[test]
fn origin_filter_selects_kinds_and_interfaces() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let legacy = create(&mut store, "written before origins");
    let interactive = create(&mut store, "interactive");
    let command = create(&mut store, "command line");
    let tool = create(&mut store, "tool server");
    for (session, origin) in [
        (&interactive, SessionOrigin::Interactive {}),
        (
            &command,
            SessionOrigin::External {
                interface: Some("cli".into()),
            },
        ),
        (
            &tool,
            SessionOrigin::External {
                interface: Some("mcp".into()),
            },
        ),
    ] {
        let metadata = SessionMetadata {
            origin: Some(origin),
            ..Default::default()
        };
        set_metadata(&path, &session.id, metadata);
    }
    let filter = |origins: Vec<OriginFilter>| SessionFilter {
        origins,
        ..Default::default()
    };
    let cli = || OriginFilter::ExternalInterface("cli".into());
    let all = ids(&store, &SessionFilter::default());
    assert_eq!(all.len(), 4);
    assert!(all.contains(&legacy.id));
    let interactive_ids = ids(&store, &filter(vec![OriginFilter::Interactive]));
    assert_eq!(interactive_ids, vec![interactive.id.clone()]);
    let external = ids(&store, &filter(vec![OriginFilter::External]));
    assert_eq!(external, vec![tool.id.clone(), command.id.clone()]);
    assert_eq!(ids(&store, &filter(vec![cli()])), vec![command.id.clone()]);
    let several = ids(&store, &filter(vec![cli(), OriginFilter::Interactive]));
    assert_eq!(several, vec![command.id, interactive.id]);
    let unused = OriginFilter::ExternalInterface("api".into());
    assert!(ids(&store, &filter(vec![unused])).is_empty());
}

#[test]
fn cursor_pages_stay_stable_under_concurrent_creates() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let mut existing: Vec<String> = (0..40)
        .map(|index| create(&mut store, &format!("existing {index}")).id)
        .collect();
    existing.reverse();
    let (started, ready) = mpsc::channel();
    let writer_path = path.clone();
    let writer = thread::spawn(move || {
        let mut writer = Store::open(&writer_path).unwrap();
        started.send(()).unwrap();
        (0..40)
            .map(|index| create(&mut writer, &format!("concurrent {index}")).id)
            .collect::<Vec<_>>()
    });
    ready.recv().unwrap();
    let filter = SessionFilter::default();
    let first = store.query_sessions(&filter, None, 7).unwrap();
    let mut seen: Vec<String> = first.sessions.iter().map(|s| s.id.clone()).collect();
    let mut cursor = first.next_cursor;
    // A second connection also commits a create between every pair of pages.
    let mut interleaved = Store::open(&path).unwrap();
    let mut between = Vec::new();
    while let Some(after) = cursor {
        between.push(create(&mut interleaved, "between pages").id);
        let page = store.query_sessions(&filter, Some(&after), 7).unwrap();
        assert!(page.sessions.len() <= 7);
        seen.extend(page.sessions.into_iter().map(|session| session.id));
        cursor = page.next_cursor;
    }
    let created = writer.join().unwrap();
    // The first page may include concurrent sessions committed before it was
    // read; every later page continues strictly below its cursor.
    let older: Vec<_> = seen.iter().filter(|id| existing.contains(id)).collect();
    assert_eq!(older, existing.iter().collect::<Vec<_>>());
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), seen.len());
    assert!(
        seen.iter()
            .all(|id| existing.contains(id) || created.contains(id))
    );
    assert!(between.iter().all(|id| !seen.contains(id)));
    let final_ids = ids(&store, &filter);
    assert_eq!(final_ids.len(), 80 + between.len());
    assert_eq!(final_ids[final_ids.len() - 40..], existing[..]);
}

#[test]
fn cursors_are_bound_to_their_list() {
    let mut store = Store::open(":memory:").unwrap();
    for index in 0..3 {
        create(&mut store, &format!("sample {index}"));
    }
    let filter = SessionFilter::default();
    let page = store.query_sessions(&filter, None, 1).unwrap();
    let cursor = page.next_cursor.unwrap();
    let next = store.query_sessions(&filter, Some(&cursor), 1).unwrap();
    assert_ne!(next.sessions, page.sessions);
    let archived = SessionFilter {
        archive: ArchiveFilter::All,
        ..Default::default()
    };
    assert_invalid(store.query_sessions(&archived, Some(&cursor), 1));
    assert_invalid(store.query_sessions(&search("sample"), Some(&cursor), 1));
    let (binding, _) = cursor.rsplit_once('.').unwrap();
    let unknown = format!("{binding}.00000000-0000-4000-8000-000000000000");
    assert_invalid(store.query_sessions(&filter, Some(&unknown), 1));
    for forged in ["", "garbage", "sl1.only", "sl0.0000000000000000.x"] {
        assert_invalid(store.query_sessions(&filter, Some(forged), 1));
    }
}

#[test]
fn title_search_is_literal_case_insensitive_and_title_only() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let titled = create(&mut store, "prompt text never searched when titled");
    let percent = create(&mut store, "Rollout reached 50% of HOSTS");
    let underscore = create(&mut store, "rename a_b");
    let long = create(&mut store, &format!("{}needle", "x".repeat(200)));
    let metadata = SessionMetadata {
        title: Some("Stored Sample Title".into()),
        ..Default::default()
    };
    set_metadata(&path, &titled.id, metadata);
    let found = |text: &str| ids(&store, &search(text));
    assert_eq!(found("sample TITLE"), vec![titled.id.clone()]);
    assert!(found("never searched").is_empty());
    assert_eq!(found("50% of hosts"), vec![percent.id.clone()]);
    assert!(found("5_%").is_empty());
    assert_eq!(found("a_b"), vec![underscore.id]);
    assert!(found("a%b").is_empty());
    assert!(found("needle").is_empty());
    assert_eq!(found(&"x".repeat(200)), vec![long.id]);
    assert_eq!(found("ROLLOUT"), vec![percent.id]);
}

#[test]
fn search_pages_continue_past_non_matching_sessions() {
    let mut store = Store::open(":memory:").unwrap();
    for index in 0..30 {
        create(&mut store, &format!("session {index}"));
    }
    let filter = search("session");
    let mut collected = Vec::new();
    let mut cursor = None;
    loop {
        let page = store.query_sessions(&filter, cursor.as_deref(), 4).unwrap();
        collected.extend(page.sessions.into_iter().map(|session| session.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(collected, ids(&store, &filter));
    assert_eq!(collected.len(), 30);
    // "session 2" and "session 20" through "session 29".
    assert_eq!(ids(&store, &search("session 2")).len(), 11);
}

#[test]
fn page_search_and_origin_bounds() {
    let mut store = Store::open(":memory:").unwrap();
    create(&mut store, "bounds");
    let all = SessionFilter::default();
    assert_invalid(store.query_sessions(&all, None, 0));
    assert_invalid(store.query_sessions(&all, None, 201));
    for page in [1, 200] {
        assert_eq!(
            store
                .query_sessions(&all, None, page)
                .unwrap()
                .sessions
                .len(),
            1
        );
    }
    assert_invalid(store.query_sessions(&search("bounds"), None, 25));
    assert!(store.query_sessions(&search("bounds"), None, 24).is_ok());
    assert_invalid(store.query_sessions(&search("ab"), None, 24));
    assert_invalid(store.query_sessions(&search(&"é".repeat(501)), None, 24));
    for text in ["abc".to_owned(), "é".repeat(500)] {
        assert!(store.query_sessions(&search(&text), None, 24).is_ok());
    }
    for name in [" ".to_owned(), "i".repeat(65)] {
        let filter = SessionFilter {
            origins: vec![OriginFilter::ExternalInterface(name)],
            ..Default::default()
        };
        assert_invalid(store.query_sessions(&filter, None, 1));
    }
}
