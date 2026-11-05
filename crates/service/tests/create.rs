use foundry_contracts::{
    Checkout, ContractError, CreateSession, SessionOrigin, SessionTarget, TargetKind,
    WorkspaceSelector,
};
use foundry_service::{Service, ServiceError, create::decode_create_request};
use foundry_store::StoreError;
use tempfile::tempdir;

const BASE: &str =
    r#""prompt":"Fix the sample test.","directory":"/workspace/sample","model":null"#;

fn decode(fields: &str) -> Result<CreateSession, ContractError> {
    decode_create_request(&format!("{{{BASE},{fields}}}"))
}

#[test]
fn exclusive_checkout_selectors_are_invalid() {
    for checkout in [
        r#"{"branch":"topic","change_request":7}"#,
        r#"{"branch":"topic","reference":"v1"}"#,
        r#"{"reference":"v1","change_request":7}"#,
        r#"{}"#,
    ] {
        let fields = format!(
            r#""idempotency_key":null,"workspace":{{"kind":"repository","repository":"sample/repo","checkout":{checkout}}}"#
        );
        assert!(
            matches!(decode(&fields), Err(ContractError::Invalid(_))),
            "{checkout}"
        );
    }
    let single = decode(
        r#""idempotency_key":null,"workspace":{"kind":"repository","repository":"sample/repo","checkout":{"change_request":7}}"#,
    )
    .unwrap();
    assert_eq!(
        single.workspace,
        Some(WorkspaceSelector::Repository {
            repository: "sample/repo".into(),
            checkout: Checkout::ChangeRequest(7),
        })
    );
    assert!(matches!(
        decode(
            r#""idempotency_key":null,"workspace":{"kind":"scratch","repository":"sample/repo"}"#
        ),
        Err(ContractError::Invalid(_))
    ));
}

#[test]
fn machine_target_without_name_is_invalid() {
    assert!(matches!(
        decode(r#""idempotency_key":null,"target":{"kind":"machine"}"#),
        Err(ContractError::Invalid(_))
    ));
    let named =
        decode(r#""idempotency_key":null,"target":{"kind":"machine","name":"builder-1"}"#).unwrap();
    assert_eq!(
        named.target,
        Some(SessionTarget {
            kind: TargetKind::Machine,
            name: "builder-1".into(),
        })
    );
    // A valid machine target is unavailable and never falls back to local execution.
    let dir = tempdir().unwrap();
    let mut service = Service::open(dir.path().join("store.db")).unwrap();
    assert!(matches!(
        service.create_session(named),
        Err(ServiceError::Store(StoreError::Contract(
            ContractError::Unsupported(_)
        )))
    ));
    assert!(service.list(10).unwrap().is_empty());
}

#[test]
fn decoded_v2_request_replays_across_restart_with_its_associations() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("store.db");
    let request = decode(concat!(
        r#""idempotency_key":"key-1","workspace":{"kind":"scratch"},"tags":["alpha"],"#,
        r#""title":"Sample","tool_profile":"default","origin":{"kind":"interactive"}"#
    ))
    .unwrap();
    let mut service = Service::open(&path).unwrap();
    let created = service.create_session(request.clone()).unwrap();
    assert!(!created.replayed);
    drop(service);

    let mut service = Service::open(&path).unwrap();
    let replay = service.create_session(request.clone()).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.session, created.session);
    let session = service.get(&created.session.id).unwrap();
    assert_eq!(session.metadata.title.as_deref(), Some("Sample"));
    assert_eq!(session.metadata.origin, Some(SessionOrigin::Interactive {}));
    let associations = service.associations(&created.session.id).unwrap();
    assert_eq!(associations, created.associations);
    assert_eq!(associations.workspace, WorkspaceSelector::Scratch {});
    assert_eq!(associations.tags, ["alpha"]);
    assert_eq!(associations.tool_profile.as_deref(), Some("default"));
    assert!(matches!(
        service.create_session(CreateSession {
            title: Some("Other".into()),
            ..request
        }),
        Err(ServiceError::Store(StoreError::Contract(
            ContractError::Conflict(_)
        )))
    ));
    assert_eq!(service.list(10).unwrap().len(), 1);
}

#[test]
fn every_create_path_resolves_the_tool_profile_before_writing() {
    let dir = tempdir().unwrap();
    let mut service = Service::open(dir.path().join("store.db")).unwrap();
    let unknown = || decode(r#""idempotency_key":null,"tool_profile":"default/read-write""#);
    let unsupported = |error| {
        matches!(
            error,
            ServiceError::Store(StoreError::Contract(ContractError::Unsupported(_)))
        )
    };
    assert!(unsupported(
        service.create_session(unknown().unwrap()).unwrap_err()
    ));
    assert!(unsupported(service.create(unknown().unwrap()).unwrap_err()));
    assert!(service.list(10).unwrap().is_empty());
    let known = decode(r#""idempotency_key":null,"tool_profile":"default""#).unwrap();
    let created = service.create_session(known).unwrap();
    assert_eq!(
        created.associations.tool_profile.as_deref(),
        Some("default")
    );
}
