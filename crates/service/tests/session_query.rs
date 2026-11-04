use foundry_contracts::{CancelMode, ContractError, CreateSession};
use foundry_service::session_query::{SessionList, SessionListPage, SessionListRequest};
use foundry_service::{Service, ServiceError};
use foundry_store::StoreError;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tempfile::{TempDir, tempdir};

fn setup(count: usize) -> (TempDir, PathBuf, Service, Vec<String>) {
    let directory = tempdir().unwrap();
    let path = directory.path().join("store.db");
    let mut service = Service::open(&path).unwrap();
    let ids = (0..count)
        .map(|index| create(&mut service, &format!("sample session {index}")))
        .collect();
    (directory, path, service, ids)
}

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

fn page(service: &Service, request: &SessionListRequest) -> SessionListPage {
    match service.list_sessions(request).unwrap() {
        SessionList::Page(page) => page,
        SessionList::NotModified { .. } => panic!("expected a page"),
    }
}

fn assert_invalid(service: &Service, request: &SessionListRequest) {
    match service.list_sessions(request) {
        Err(ServiceError::Store(StoreError::Contract(ContractError::Invalid(_)))) => {}
        other => panic!("expected invalid_request, got {other:?}"),
    }
}

fn archive(path: &Path, id: &str) {
    let db = Connection::open(path).unwrap();
    db.execute(
        "UPDATE sessions SET archived=1, body=json_set(body,'$.archived',json('true')) WHERE id=?1",
        [id],
    )
    .unwrap();
}

#[test]
fn page_size_defaults_caps_and_alias() {
    let (_directory, _path, service, _) = setup(230);
    let sized = |page_size, limit| SessionListRequest {
        page_size,
        limit,
        ..Default::default()
    };
    let default = page(&service, &SessionListRequest::default());
    assert_eq!((default.effective_page, default.sessions.len()), (50, 50));
    for (request, expected) in [
        (sized(None, Some(500)), 200),
        (sized(Some(500), None), 200),
        (sized(Some(201), None), 200),
        (sized(Some(200), None), 200),
        (sized(Some(1), None), 1),
        (sized(Some(10), Some(30)), 10),
        (sized(None, Some(30)), 30),
    ] {
        let listed = page(&service, &request);
        assert_eq!(listed.effective_page, expected);
        assert_eq!(listed.sessions.len(), expected as usize);
    }
    assert_invalid(&service, &sized(Some(0), None));
    assert_invalid(&service, &sized(None, Some(0)));
    assert_invalid(&service, &sized(Some(0), Some(30)));
    let searched = |limit| SessionListRequest {
        search: Some("sample".into()),
        limit,
        ..Default::default()
    };
    for (limit, expected) in [(None, 24), (Some(50), 24), (Some(25), 24), (Some(3), 3)] {
        let listed = page(&service, &searched(limit));
        assert_eq!(listed.effective_page, expected);
        assert_eq!(listed.sessions.len(), expected as usize);
    }
    let short = SessionListRequest {
        search: Some("ab".into()),
        ..Default::default()
    };
    assert_invalid(&service, &short);
}

#[test]
fn sparse_fields_always_keep_the_session_id() {
    let (_directory, path, service, ids) = setup(2);
    archive(&path, &ids[0]);
    let request = SessionListRequest {
        archive: foundry_service::session_query::ArchiveFilter::All,
        fields: vec!["lifecycle".into(), "archived".into(), "lifecycle".into()],
        ..Default::default()
    };
    let listed = page(&service, &request);
    let rows: Vec<Value> = listed.sessions.into_iter().map(Value::Object).collect();
    assert_eq!(
        rows,
        vec![
            json!({"id": ids[1], "lifecycle": "queued", "archived": false}),
            json!({"id": ids[0], "lifecycle": "queued", "archived": true}),
        ]
    );
    let title = SessionListRequest {
        fields: vec!["title".into()],
        ..Default::default()
    };
    let listed = page(&service, &title);
    assert_eq!(
        Value::Object(listed.sessions[0].clone()),
        json!({"id": ids[1], "title": null})
    );
    let full = page(&service, &SessionListRequest::default());
    assert_eq!(full.sessions[0]["prompt"], "sample session 1");
    let unknown = SessionListRequest {
        fields: vec!["not_a_field".into()],
        ..Default::default()
    };
    assert_invalid(&service, &unknown);
}

#[test]
fn etag_is_stable_until_a_visible_change() {
    let (_directory, path, mut service, ids) = setup(3);
    let first_only = SessionListRequest {
        page_size: Some(1),
        ..Default::default()
    };
    let before = page(&service, &first_only);
    assert!(before.etag.starts_with("W/\""));
    assert_eq!(page(&service, &first_only).etag, before.etag);
    let conditional = SessionListRequest {
        if_none_match: Some(before.etag.clone()),
        ..first_only.clone()
    };
    assert_eq!(
        service.list_sessions(&conditional).unwrap(),
        SessionList::NotModified {
            etag: before.etag.clone()
        }
    );
    // The oldest session is not on this page; changing it is not visible.
    service.cancel(&ids[0], CancelMode::Graceful).unwrap();
    assert_eq!(page(&service, &first_only).etag, before.etag);
    // A field the caller did not select is not visible either.
    let titles = SessionListRequest {
        fields: vec!["title".into()],
        ..first_only.clone()
    };
    let titled = page(&service, &titles).etag;
    assert_ne!(titled, before.etag);
    service.cancel(&ids[2], CancelMode::Graceful).unwrap();
    assert_eq!(page(&service, &titles).etag, titled);
    let changed = page(&service, &first_only);
    assert_ne!(changed.etag, before.etag);
    assert!(matches!(
        service.list_sessions(&conditional).unwrap(),
        SessionList::Page(_)
    ));
    // New sessions and archive changes alter the page too.
    let newest = create(&mut service, "newest");
    let after_create = page(&service, &first_only);
    assert_eq!(after_create.sessions[0]["id"], newest.as_str());
    assert_ne!(after_create.etag, changed.etag);
    archive(&path, &newest);
    assert_eq!(page(&service, &first_only).etag, changed.etag);
    // The validator covers the query: another page or filter has its own.
    let next = SessionListRequest {
        after: changed.next_cursor.clone(),
        ..first_only.clone()
    };
    assert_ne!(page(&service, &next).etag, changed.etag);
}

#[test]
fn cursor_paging_walks_every_session_once() {
    let (_directory, _path, mut service, mut ids) = setup(9);
    ids.reverse();
    let mut request = SessionListRequest {
        page_size: Some(4),
        ..Default::default()
    };
    let mut seen = Vec::new();
    loop {
        let listed = page(&service, &request);
        create(&mut service, "created while paging");
        seen.extend(
            listed
                .sessions
                .iter()
                .map(|row| row["id"].as_str().unwrap().to_owned()),
        );
        match listed.next_cursor {
            Some(cursor) => request.after = Some(cursor),
            None => break,
        }
    }
    assert_eq!(seen, ids);
    let forged = SessionListRequest {
        after: Some("sl1.0000000000000000.missing".into()),
        ..Default::default()
    };
    assert_invalid(&service, &forged);
}
