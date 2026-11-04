use foundry_contracts::{
    Checkout, CompletionCriteria, ContractError, CreateSession, ModelConfig, SessionOrigin,
    SessionTarget, TargetKind, WorkspaceSelector,
};
use foundry_store::{SessionAssociations, Store, StoreError};
use rusqlite::Connection;
use std::path::Path;
use tempfile::tempdir;

fn full_request() -> CreateSession {
    CreateSession {
        prompt: "Fix the sample test.".into(),
        directory: "/workspace/sample".into(),
        model: Some(ModelConfig {
            provider: "local".into(),
            model: "deterministic".into(),
            effort: None,
        }),
        idempotency_key: Some("key-v2".into()),
        workspace: Some(WorkspaceSelector::Repository {
            repository: "sample/repo".into(),
            checkout: Checkout::Branch("topic".into()),
        }),
        tags: vec!["beta".into(), "alpha".into()],
        title: Some("Sample cleanup".into()),
        tool_profile: Some("default/read-only".into()),
        origin: Some(SessionOrigin::External {
            interface: Some("mcp".into()),
        }),
        ..Default::default()
    }
}

fn legacy_request() -> CreateSession {
    CreateSession {
        prompt: "Draft a plan.".into(),
        directory: "/workspace/sample".into(),
        idempotency_key: Some("key-legacy".into()),
        ..Default::default()
    }
}

fn session_rows(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap()
}

fn invalid(result: Result<impl std::fmt::Debug, StoreError>) -> bool {
    matches!(result, Err(StoreError::Contract(ContractError::Invalid(_))))
}

fn unsupported(result: Result<impl std::fmt::Debug, StoreError>) -> bool {
    matches!(
        result,
        Err(StoreError::Contract(ContractError::Unsupported(_)))
    )
}

#[test]
fn v2_options_persist_in_columns_tags_and_the_stored_request() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let request = full_request();
    let receipt = store.create_session(request.clone()).unwrap();
    assert!(!receipt.replayed);
    let expected = SessionAssociations {
        workspace: request.workspace.clone().unwrap(),
        tags: vec!["beta".into(), "alpha".into()],
        tool_profile: Some("default/read-only".into()),
    };
    assert_eq!(receipt.associations, expected);
    assert_eq!(
        receipt.session.metadata.title.as_deref(),
        Some("Sample cleanup")
    );
    assert_eq!(receipt.session.metadata.origin, request.origin);
    drop(store);

    let store = Store::open(&path).unwrap();
    assert_eq!(store.get(&receipt.session.id).unwrap(), receipt.session);
    assert_eq!(store.associations(&receipt.session.id).unwrap(), expected);
    let db = Connection::open(&path).unwrap();
    let columns: (String, String, String, String) = db
        .query_row(
            "SELECT creation, title, origin_kind, origin_interface FROM sessions WHERE id=?1",
            [&receipt.session.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        columns,
        (
            serde_json::to_string(&request).unwrap(),
            "Sample cleanup".into(),
            "external".into(),
            "mcp".into()
        )
    );
    let tags: Vec<(String, i64)> = db
        .prepare("SELECT tag, position FROM session_tags ORDER BY position")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(tags, [("beta".into(), 0), ("alpha".into(), 1)]);
    assert!(matches!(
        store.associations("missing"),
        Err(StoreError::Contract(ContractError::NotFound(_)))
    ));
}

#[test]
fn scratch_is_explicit_or_the_default_and_legacy_bodies_gain_no_fields() {
    let dir = tempdir().unwrap();
    let mut store = Store::open(dir.path().join("store.db")).unwrap();
    let scratch = store
        .create_session(CreateSession {
            workspace: Some(WorkspaceSelector::Scratch {}),
            origin: Some(SessionOrigin::Interactive {}),
            idempotency_key: None,
            ..legacy_request()
        })
        .unwrap();
    let legacy = store.create_session(legacy_request()).unwrap();
    for receipt in [&scratch, &legacy] {
        assert_eq!(
            store.associations(&receipt.session.id).unwrap(),
            SessionAssociations {
                workspace: WorkspaceSelector::Scratch {},
                tags: vec![],
                tool_profile: None,
            }
        );
    }
    // A request without v2 options stores a session body in the pre-v2 shape.
    let body = serde_json::to_value(&legacy.session).unwrap();
    let keys: Vec<_> = body.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        [
            "active_attempt_id",
            "directory",
            "id",
            "lifecycle",
            "model",
            "outcome",
            "prompt",
            "result",
            "version"
        ]
    );
}

#[test]
fn legacy_and_v2_requests_replay_after_restart_and_conflict_on_any_changed_field() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    let legacy = store.create_session(legacy_request()).unwrap();
    let full = store.create_session(full_request()).unwrap();
    drop(store);

    let mut store = Store::open(&path).unwrap();
    for (request, original) in [(legacy_request(), &legacy), (full_request(), &full)] {
        let replay = store.create_session(request).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.session, original.session);
        assert_eq!(replay.associations, original.associations);
    }
    assert_eq!(store.create(full_request()).unwrap(), full.session);

    type Change = fn(&mut CreateSession);
    let changes: [Change; 12] = [
        |r| r.prompt = "Fix another test.".into(),
        |r| r.directory = "/workspace/other".into(),
        |r| r.model = None,
        |r| r.workspace = None,
        |r| r.workspace = Some(WorkspaceSelector::Scratch {}),
        |r| {
            r.workspace = Some(WorkspaceSelector::Repository {
                repository: "sample/repo".into(),
                checkout: Checkout::ChangeRequest(7),
            })
        },
        |r| r.tags.reverse(),
        |r| r.tags.push("gamma".into()),
        |r| r.title = None,
        |r| r.tool_profile = Some("default".into()),
        |r| r.origin = Some(SessionOrigin::Interactive {}),
        |r| {
            r.origin = Some(SessionOrigin::External { interface: None });
        },
    ];
    for (index, change) in changes.iter().enumerate() {
        let mut request = full_request();
        change(&mut request);
        assert!(
            matches!(
                store.create_session(request),
                Err(StoreError::Contract(ContractError::Conflict(_)))
            ),
            "change {index}"
        );
    }
    let mut widened = legacy_request();
    widened.title = Some("Draft".into());
    assert!(matches!(
        store.create_session(widened),
        Err(StoreError::Contract(ContractError::Conflict(_)))
    ));
    assert_eq!(session_rows(&path), 2);
    assert_eq!(
        store.associations(&full.session.id).unwrap(),
        full.associations
    );
}

#[test]
fn every_rejection_is_typed_and_writes_nothing() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store.db");
    let mut store = Store::open(&path).unwrap();
    type Change = fn(&mut CreateSession);
    let invalid_changes: [Change; 16] = [
        |r| r.prompt = " ".into(),
        |r| r.tags = vec!["a".into(), "a".into()],
        |r| r.tags = (0..65).map(|n| n.to_string()).collect(),
        |r| r.tags = vec!["x".repeat(65)],
        |r| r.title = Some(" ".into()),
        |r| r.title = Some("t".repeat(201)),
        |r| {
            r.workspace = Some(WorkspaceSelector::Repository {
                repository: " ".into(),
                checkout: Checkout::Reference("v1".into()),
            })
        },
        |r| {
            r.workspace = Some(WorkspaceSelector::Repository {
                repository: "sample/repo".into(),
                checkout: Checkout::ChangeRequest(0),
            })
        },
        |r| {
            r.target = Some(SessionTarget {
                kind: TargetKind::Machine,
                name: " ".into(),
            })
        },
        |r| r.tool_profile = Some(String::new()),
        |r| r.tool_profile = Some("Default".into()),
        |r| r.tool_profile = Some("read only".into()),
        |r| r.tool_profile = Some("default/".into()),
        |r| r.tool_profile = Some("../default".into()),
        |r| r.tool_profile = Some("x".repeat(257)),
        |r| {
            r.origin = Some(SessionOrigin::External {
                interface: Some(String::new()),
            })
        },
    ];
    for (index, change) in invalid_changes.iter().enumerate() {
        let mut request = full_request();
        change(&mut request);
        assert!(invalid(store.create_session(request)), "invalid {index}");
    }
    let machine = CreateSession {
        target: Some(SessionTarget {
            kind: TargetKind::Machine,
            name: "builder-1".into(),
        }),
        ..full_request()
    };
    assert!(unsupported(store.create_session(machine.clone())));
    // Completion stays unsupported until its owner stores it.
    let completion = CreateSession {
        completion: Some(CompletionCriteria {
            profile: Some("tests".into()),
            checks: vec![],
        }),
        ..full_request()
    };
    assert!(unsupported(store.create(completion)));
    // An invalid option is reported before an unavailable target.
    assert!(invalid(store.create_session(CreateSession {
        tool_profile: Some("Default".into()),
        ..machine
    })));
    assert_eq!(session_rows(&path), 0);
    assert!(store.create_session(full_request()).is_ok());
}

#[test]
fn v11_creation_strings_replay_byte_identically_through_the_v2_path() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(include_str!("fixtures/store-v11.sql"))
        .unwrap();
    let mut store = Store::open(&path).unwrap();
    let request = CreateSession {
        prompt: "Draft a plan.".into(),
        directory: "/workspace/sample".into(),
        idempotency_key: Some("fixture-key-2".into()),
        ..Default::default()
    };
    let (creation, body): (String, String) = db
        .query_row(
            "SELECT creation, body FROM sessions WHERE idempotency_key='fixture-key-2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(serde_json::to_string(&request).unwrap(), creation);
    let replay = store.create_session(request).unwrap();
    assert!(replay.replayed);
    assert_eq!(serde_json::to_string(&replay.session).unwrap(), body);
    assert_eq!(
        replay.associations,
        SessionAssociations {
            workspace: WorkspaceSelector::Scratch {},
            tags: vec![],
            tool_profile: None,
        }
    );
    assert_eq!(session_rows(&path), 3);
}
