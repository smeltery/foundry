use foundry_contracts::{ContractError, CreateSession, Lifecycle, Outcome};
use foundry_service::session_meta::{DeleteMode, DeleteOutcome, display_title};
use foundry_service::session_query::{ArchiveFilter, SessionList, SessionListRequest};
use foundry_service::{Service, ServiceError};
use foundry_store::{Store, StoreError};
use std::{
    path::Path,
    sync::{Arc, Barrier},
    thread,
};
use tempfile::tempdir;

fn create(service: &mut Service, prompt: &str) -> String {
    service
        .create(CreateSession {
            prompt: prompt.into(),
            directory: "/workspace/project".into(),
            ..Default::default()
        })
        .unwrap()
        .id
}

fn listed(service: &Service, archive: ArchiveFilter) -> Vec<String> {
    let request = SessionListRequest {
        archive,
        ..Default::default()
    };
    match service.list_sessions(&request).unwrap() {
        SessionList::Page(page) => page
            .sessions
            .into_iter()
            .map(|session| session["id"].as_str().unwrap().to_owned())
            .collect(),
        SessionList::NotModified { .. } => panic!("expected a page"),
    }
}

fn error_class<T>(result: foundry_service::Result<T>) -> &'static str {
    match result {
        Err(ServiceError::Store(StoreError::Contract(error))) => match error {
            ContractError::Invalid(_) => "invalid_request",
            ContractError::NotFound(_) => "not_found",
            ContractError::Conflict(_) => "conflict",
            _ => "other",
        },
        Err(other) => panic!("unexpected error {other}"),
        Ok(_) => "ok",
    }
}

fn tags(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// A live attempt owned by another connection, as another process would hold it.
fn settle(path: &Path, id: &str) {
    let mut owner = Store::open(path).unwrap();
    let fence = owner.begin_attempt(id, 0).unwrap();
    owner
        .finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Failure), None)
        .unwrap();
}

#[test]
fn archive_and_unarchive_are_repeatable_and_keep_lifecycle() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut service = Service::open(&path).unwrap();
    let id = create(&mut service, "archive sample");
    settle(&path, &id);
    let before = service.get(&id).unwrap();
    for _ in 0..2 {
        let session = service.archive(&id).unwrap();
        assert!(session.metadata.archived);
        assert_eq!(
            (session.lifecycle, session.outcome, session.version),
            (before.lifecycle, before.outcome, before.version)
        );
    }
    assert!(listed(&service, ArchiveFilter::Active).is_empty());
    assert_eq!(
        listed(&service, ArchiveFilter::Archived),
        std::slice::from_ref(&id)
    );
    for _ in 0..2 {
        assert_eq!(service.unarchive(&id).unwrap(), before);
    }
    assert_eq!(listed(&service, ArchiveFilter::Active), [id]);
}

#[test]
fn rename_reports_the_stored_or_derived_title() {
    let dir = tempdir().unwrap();
    let mut service = Service::open(dir.path().join("db")).unwrap();
    let id = create(&mut service, "Summarize the sample notes.");
    let renamed = service.rename(&id, Some("Sample cleanup")).unwrap();
    assert_eq!(renamed.title, "Sample cleanup");
    assert_eq!(service.get(&id).unwrap(), renamed.session);
    let cleared = service.rename(&id, Some("")).unwrap();
    assert_eq!(cleared.title, "Summarize the sample notes.");
    assert_eq!(cleared.session.metadata.title, None);
    assert_eq!(display_title(&service.get(&id).unwrap()), cleared.title);
    assert_eq!(
        error_class(service.rename(&id, Some(" "))),
        "invalid_request"
    );
    assert_eq!(error_class(service.rename("missing", None)), "not_found");
}

#[test]
fn tag_operations_follow_the_metadata_vectors() {
    let dir = tempdir().unwrap();
    let mut service = Service::open(dir.path().join("db")).unwrap();
    let id = create(&mut service, "tag sample");
    assert!(service.tags(&id).unwrap().is_empty());
    service.append_tags(&id, &tags(&["alpha", "beta"])).unwrap();
    assert_eq!(
        service.append_tags(&id, &tags(&["beta", "gamma"])).unwrap(),
        ["alpha", "beta", "gamma"]
    );
    assert_eq!(
        service.replace_tags(&id, &tags(&["gamma"])).unwrap(),
        ["gamma"]
    );
    assert!(service.replace_tags(&id, &[]).unwrap().is_empty());
    assert_eq!(
        service.associations(&id).unwrap().tags,
        Vec::<String>::new()
    );
    assert_eq!(
        error_class(service.append_tags(&id, &tags(&[""]))),
        "invalid_request"
    );
}

#[test]
fn concurrent_services_patch_tags_atomically() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let id = create(&mut Service::open(&path).unwrap(), "race");
    let writers = 4;
    let barrier = Arc::new(Barrier::new(writers));
    let handles: Vec<_> = (0..writers)
        .map(|writer| {
            let (path, id, barrier) = (path.clone(), id.clone(), barrier.clone());
            thread::spawn(move || {
                let mut service = Service::open(&path).unwrap();
                barrier.wait();
                for n in 0..12 {
                    service
                        .append_tags(&id, &[format!("w{writer}-{n}")])
                        .unwrap();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    let mut stored = Service::open(&path).unwrap().tags(&id).unwrap();
    stored.sort();
    let mut expected: Vec<_> = (0..writers)
        .flat_map(|writer| (0..12).map(move |n| format!("w{writer}-{n}")))
        .collect();
    expected.sort();
    assert_eq!(stored, expected);
}

#[test]
fn delete_refuses_running_preserve_archives_and_tombstone_hides() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("db");
    let mut service = Service::open(&path).unwrap();
    let id = create(&mut service, "delete sample");
    let mut owner = Store::open(&path).unwrap();
    let fence = owner.begin_attempt(&id, 0).unwrap();
    assert_eq!(
        error_class(service.delete(&id, DeleteMode::Tombstone)),
        "conflict"
    );
    assert_eq!(service.get(&id).unwrap().lifecycle, Lifecycle::Running);
    // Preserve only archives, which never touches execution.
    match service.delete(&id, DeleteMode::Preserve).unwrap() {
        DeleteOutcome::Archived(session) => {
            assert!(session.metadata.archived);
            assert_eq!(session.lifecycle, Lifecycle::Running);
        }
        other => panic!("expected archive, got {other:?}"),
    }
    owner
        .finish_attempt(&fence, Lifecycle::Exit, Some(Outcome::Failure), None)
        .unwrap();
    match service.delete(&id, DeleteMode::Tombstone).unwrap() {
        DeleteOutcome::Deleted(receipt) => assert_eq!(receipt.session_id, id),
        other => panic!("expected tombstone, got {other:?}"),
    }
    assert_eq!(error_class(service.get(&id)), "not_found");
    assert_eq!(error_class(service.events(&id, 0, 10)), "not_found");
    assert_eq!(
        error_class(service.delete(&id, DeleteMode::Tombstone)),
        "not_found"
    );
    assert!(listed(&service, ArchiveFilter::All).is_empty());
    assert!(service.list(10).unwrap().is_empty());
}
