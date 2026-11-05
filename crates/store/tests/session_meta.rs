//! Archive, rename and tags (session-api.md, "Archive, unarchive and delete"
//! and "Title, tags and project grouping").
use foundry_contracts::{ContractError, CreateSession, Lifecycle, Outcome};
use foundry_store::{ArchiveFilter, SessionFilter, Store, StoreError};
use serde_json::json;

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

fn invalid<T>(result: foundry_store::Result<T>) {
    match result {
        Err(StoreError::Contract(ContractError::Invalid(_))) => {}
        Err(other) => panic!("expected invalid_request, got {other}"),
        Ok(_) => panic!("expected invalid_request, got success"),
    }
}

#[test]
fn archive_and_unarchive_change_listing_but_not_lifecycle_outcome_or_version() {
    let mut store = Store::open(":memory:").unwrap();
    let id = store.create(request("settled sample")).unwrap().id;
    let fence = store.begin_attempt(&id, 0).unwrap();
    store
        .finish_attempt(
            &fence,
            Lifecycle::Exit,
            Some(Outcome::Failure),
            Some("r".into()),
        )
        .unwrap();
    let before = store.get(&id).unwrap();
    for _ in 0..2 {
        let archived = store.set_archived(&id, true).unwrap();
        assert!(archived.metadata.archived);
        assert_eq!(
            (archived.lifecycle, archived.outcome, archived.version),
            (Lifecycle::Exit, Some(Outcome::Failure), before.version)
        );
        assert_eq!(archived.result, before.result);
        assert_eq!(store.get(&id).unwrap(), archived);
    }
    assert!(listed(&store, ArchiveFilter::Active).is_empty());
    assert_eq!(
        listed(&store, ArchiveFilter::Archived),
        std::slice::from_ref(&id)
    );
    for _ in 0..2 {
        assert_eq!(store.set_archived(&id, false).unwrap(), before);
    }
    assert_eq!(store.get(&id).unwrap(), before);
    assert_eq!(listed(&store, ArchiveFilter::Active), [id]);
    not_found(store.set_archived("missing", true));
}

#[test]
fn metadata_writes_during_execution_leave_the_attempt_and_its_fence_alone() {
    let mut store = Store::open(":memory:").unwrap();
    let queued = store.create(request("queued sample")).unwrap().id;
    store.set_archived(&queued, true).unwrap();
    store.rename_session(&queued, Some("Queued")).unwrap();
    // Admission still expects the creation version: metadata is not execution state.
    let fence = store.begin_attempt(&queued, 0).unwrap();
    store.set_archived(&queued, false).unwrap();
    store.set_archived(&queued, true).unwrap();
    store.append_tags(&queued, &tags(&["live"])).unwrap();
    let running = store.get(&queued).unwrap();
    assert_eq!(running.lifecycle, Lifecycle::Running);
    assert_eq!(
        running.active_attempt_id.as_deref(),
        Some(fence.attempt_id.as_str())
    );
    store.heartbeat(&fence).unwrap();
    store
        .append_event(&fence, "e1", 0, "text", json!("still running"))
        .unwrap();
    let settled = store
        .finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Failure), None)
        .unwrap();
    assert_eq!(settled.lifecycle, Lifecycle::Exit);
    // The terminal write keeps metadata written while the attempt ran.
    assert!(settled.metadata.archived);
    assert_eq!(settled.metadata.title.as_deref(), Some("Queued"));
    assert_eq!(store.session_tags(&queued).unwrap(), ["live"]);
}

#[test]
fn rename_sets_clears_and_validates_the_stored_title() {
    let mut store = Store::open(":memory:").unwrap();
    let id = store
        .create(request("Summarize the sample notes."))
        .unwrap()
        .id;
    let version = store.get(&id).unwrap().version;
    let renamed = store.rename_session(&id, Some("Sample cleanup")).unwrap();
    assert_eq!(renamed.metadata.title.as_deref(), Some("Sample cleanup"));
    assert_eq!(renamed.version, version);
    let search = SessionFilter {
        title_search: Some("cleanup".into()),
        ..Default::default()
    };
    assert_eq!(
        store.query_sessions(&search, None, 24).unwrap().sessions,
        [renamed]
    );
    for cleared in [Some(""), None] {
        let session = store.rename_session(&id, cleared).unwrap();
        assert_eq!(session.metadata.title, None);
        assert!(
            store
                .query_sessions(&search, None, 24)
                .unwrap()
                .sessions
                .is_empty()
        );
        store.rename_session(&id, Some("Sample cleanup")).unwrap();
    }
    let long = "x".repeat(201);
    for bad in ["   ", long.as_str()] {
        invalid(store.rename_session(&id, Some(bad)));
    }
    assert_eq!(
        store.get(&id).unwrap().metadata.title.as_deref(),
        Some("Sample cleanup")
    );
    not_found(store.rename_session("missing", Some("title")));
}

#[test]
fn tags_list_append_and_replace_return_the_normalized_stored_list() {
    let mut store = Store::open(":memory:").unwrap();
    let id = store.create(request("tag sample")).unwrap().id;
    assert!(store.session_tags(&id).unwrap().is_empty());
    assert_eq!(
        store.append_tags(&id, &tags(&["alpha", "beta"])).unwrap(),
        ["alpha", "beta"]
    );
    assert_eq!(
        store
            .append_tags(&id, &tags(&["beta", "gamma", "gamma"]))
            .unwrap(),
        ["alpha", "beta", "gamma"]
    );
    assert_eq!(store.session_tags(&id).unwrap(), ["alpha", "beta", "gamma"]);
    assert_eq!(
        store
            .replace_tags(&id, &tags(&["gamma", "delta", "gamma"]))
            .unwrap(),
        ["gamma", "delta"]
    );
    assert_eq!(store.associations(&id).unwrap().tags, ["gamma", "delta"]);
    assert!(store.replace_tags(&id, &[]).unwrap().is_empty());
    assert!(store.session_tags(&id).unwrap().is_empty());

    let full: Vec<String> = (0..64).map(|n| format!("t{n}")).collect();
    assert_eq!(store.replace_tags(&id, &full).unwrap(), full);
    invalid(store.append_tags(&id, &tags(&["overflow"])));
    for bad in [" ", &"x".repeat(65)] {
        invalid(store.append_tags(&id, &[bad.to_owned()]));
        invalid(store.replace_tags(&id, &[bad.to_owned()]));
    }
    // A rejected request writes nothing.
    assert_eq!(store.session_tags(&id).unwrap(), full);
    not_found(store.session_tags("missing"));
    not_found(store.append_tags("missing", &tags(&["a"])));
}
