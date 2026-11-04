use foundry_contracts::{
    CancelMode, CancelReceipt, Checkout, CompletionCheck, CompletionCriteria, CreateSession,
    Lifecycle, ModelConfig, Session, SessionMetadata, SessionOrigin, SessionTarget, TargetKind,
    WorkspaceSelector,
};

const LEGACY_CREATE: &str = r#"{"prompt":"Draft a plan.","directory":"/workspace/sample","model":{"provider":"local","model":"deterministic","effort":null},"idempotency_key":"key-1"}"#;
const FULL_CREATE: &str = concat!(
    r#"{"prompt":"Fix the sample test.","directory":"/workspace/sample","model":null,"idempotency_key":null,"#,
    r#""workspace":{"kind":"repository","repository":"sample/repo","checkout":{"branch":"topic"}},"#,
    r#""tags":["alpha","beta"],"title":"Sample cleanup","target":{"kind":"machine","name":"builder-1"},"#,
    r#""tool_profile":"default","origin":{"kind":"external","interface":"mcp"},"#,
    r#""completion":{"profile":"tests","checks":["#,
    r#"{"kind":"command","id":"unit","argv":["cargo","test"],"timeout_ms":60000},"#,
    r#"{"kind":"file","id":"report","path":"out/report.txt","sha256":"#,
    r#""6e4ddb1e2d01c811f83a059483e0853b2b74c59fd1e90327144c3e09c4ed5615"}]}}"#
);
const LEGACY_SESSION: &str = r#"{"id":"s-1","prompt":"Draft a plan.","directory":"/workspace/sample","model":null,"lifecycle":"queued","outcome":null,"result":null,"active_attempt_id":null,"version":0}"#;

fn legacy_create() -> CreateSession {
    CreateSession {
        prompt: "Draft a plan.".into(),
        directory: "/workspace/sample".into(),
        model: Some(ModelConfig {
            provider: "local".into(),
            model: "deterministic".into(),
            effort: None,
        }),
        idempotency_key: Some("key-1".into()),
        ..Default::default()
    }
}

fn full_create() -> CreateSession {
    CreateSession {
        prompt: "Fix the sample test.".into(),
        directory: "/workspace/sample".into(),
        workspace: Some(WorkspaceSelector::Repository {
            repository: "sample/repo".into(),
            checkout: Checkout::Branch("topic".into()),
        }),
        tags: vec!["alpha".into(), "beta".into()],
        title: Some("Sample cleanup".into()),
        target: Some(SessionTarget {
            kind: TargetKind::Machine,
            name: "builder-1".into(),
        }),
        tool_profile: Some("default".into()),
        origin: Some(SessionOrigin::External {
            interface: Some("mcp".into()),
        }),
        completion: Some(CompletionCriteria {
            profile: Some("tests".into()),
            checks: vec![
                CompletionCheck::Command {
                    id: "unit".into(),
                    argv: vec!["cargo".into(), "test".into()],
                    timeout_ms: 60_000,
                },
                CompletionCheck::File {
                    id: "report".into(),
                    path: "out/report.txt".into(),
                    sha256: Some(
                        "6e4ddb1e2d01c811f83a059483e0853b2b74c59fd1e90327144c3e09c4ed5615".into(),
                    ),
                },
            ],
        }),
        ..Default::default()
    }
}

type Mutation = Box<dyn Fn(&mut CreateSession)>;

fn decode(json: &str) -> Result<CreateSession, serde_json::Error> {
    serde_json::from_str(json)
}

#[test]
fn absent_v2_options_keep_the_legacy_create_bytes() {
    let request = legacy_create();
    assert_eq!(serde_json::to_string(&request).unwrap(), LEGACY_CREATE);
    assert_eq!(decode(LEGACY_CREATE).unwrap(), request);
    assert!(request.v2_options().is_empty());
    request.validate().unwrap();
}

#[test]
fn full_create_has_a_golden_encoding_and_round_trips() {
    let request = full_create();
    assert_eq!(serde_json::to_string(&request).unwrap(), FULL_CREATE);
    assert_eq!(decode(FULL_CREATE).unwrap(), request);
    request.validate().unwrap();
    assert_eq!(
        request.v2_options(),
        [
            "workspace",
            "tags",
            "title",
            "target",
            "tool_profile",
            "origin",
            "completion"
        ]
    );
}

#[test]
fn selector_origin_and_check_variants_have_golden_encodings() {
    let cases = [
        (
            serde_json::to_string(&WorkspaceSelector::Scratch {}).unwrap(),
            r#"{"kind":"scratch"}"#,
        ),
        (
            serde_json::to_string(&Checkout::Reference("v1.2".into())).unwrap(),
            r#"{"reference":"v1.2"}"#,
        ),
        (
            serde_json::to_string(&Checkout::ChangeRequest(7)).unwrap(),
            r#"{"change_request":7}"#,
        ),
        (
            serde_json::to_string(&SessionOrigin::Interactive {}).unwrap(),
            r#"{"kind":"interactive"}"#,
        ),
        (
            serde_json::to_string(&SessionOrigin::External { interface: None }).unwrap(),
            r#"{"kind":"external"}"#,
        ),
        (
            serde_json::to_string(&CompletionCheck::File {
                id: "exists".into(),
                path: "README.md".into(),
                sha256: None,
            })
            .unwrap(),
            r#"{"kind":"file","id":"exists","path":"README.md"}"#,
        ),
    ];
    for (actual, expected) in cases {
        assert_eq!(actual, expected);
    }
}

#[test]
fn legacy_session_bytes_are_unchanged_and_metadata_is_flat() {
    let session: Session = serde_json::from_str(LEGACY_SESSION).unwrap();
    assert_eq!(*session.metadata, SessionMetadata::default());
    assert_eq!(serde_json::to_string(&session).unwrap(), LEGACY_SESSION);

    let mut annotated = session;
    *annotated.metadata = SessionMetadata {
        archived: true,
        title: Some("Sample cleanup".into()),
        origin: Some(SessionOrigin::Interactive {}),
        created_ms: Some(1_700_000_000_000),
        updated_ms: Some(1_700_000_000_500),
        parent_session_id: Some("p-1".into()),
        forked_from_session_id: Some("f-1".into()),
    };
    let expected = concat!(
        r#"{"id":"s-1","prompt":"Draft a plan.","directory":"/workspace/sample","model":null,"#,
        r#""lifecycle":"queued","outcome":null,"result":null,"active_attempt_id":null,"version":0,"#,
        r#""archived":true,"title":"Sample cleanup","origin":{"kind":"interactive"},"#,
        r#""created_ms":1700000000000,"updated_ms":1700000000500,"#,
        r#""parent_session_id":"p-1","forked_from_session_id":"f-1"}"#
    );
    let encoded = serde_json::to_string(&annotated).unwrap();
    assert_eq!(encoded, expected);
    assert_eq!(
        serde_json::from_str::<Session>(&encoded).unwrap(),
        annotated
    );
}

#[test]
fn cancel_receipt_has_a_golden_encoding() {
    let receipt = CancelReceipt {
        ok: true,
        requested: false,
        mode: CancelMode::Hard,
        status: Lifecycle::Running,
    };
    let encoded = r#"{"ok":true,"requested":false,"mode":"hard","status":"running"}"#;
    assert_eq!(serde_json::to_string(&receipt).unwrap(), encoded);
    assert_eq!(
        serde_json::from_str::<CancelReceipt>(encoded).unwrap(),
        receipt
    );
}

#[test]
fn cancelled_lifecycle_is_additive_and_existing_encodings_are_unchanged() {
    for (lifecycle, encoded) in [
        (Lifecycle::Queued, "\"queued\""),
        (Lifecycle::Running, "\"running\""),
        (Lifecycle::Exit, "\"exit\""),
        (Lifecycle::Error, "\"error\""),
        (Lifecycle::Suspended, "\"suspended\""),
        (Lifecycle::Cancelled, "\"cancelled\""),
    ] {
        assert_eq!(serde_json::to_string(&lifecycle).unwrap(), encoded);
        assert_eq!(
            serde_json::from_str::<Lifecycle>(encoded).unwrap(),
            lifecycle
        );
    }
    let receipt = CancelReceipt {
        ok: true,
        requested: true,
        mode: CancelMode::Graceful,
        status: Lifecycle::Cancelled,
    };
    assert_eq!(
        serde_json::to_string(&receipt).unwrap(),
        r#"{"ok":true,"requested":true,"mode":"graceful","status":"cancelled"}"#
    );
}

#[test]
fn unknown_conflicting_or_incomplete_fields_are_rejected_when_decoding() {
    let base = r#""prompt":"p","directory":"/d","model":null,"idempotency_key":null"#;
    let rejected = [
        r#""unknown":true"#,
        r#""workspace":{"kind":"scratch","repository":"sample/repo"}"#,
        r#""workspace":{"kind":"repository","repository":"r","checkout":{"branch":"a","reference":"b"}}"#,
        r#""workspace":{"kind":"repository","repository":"r","checkout":{"commit":"a"}}"#,
        r#""workspace":{"kind":"repository","repository":"r"}"#,
        r#""workspace":{"kind":"volume"}"#,
        r#""target":{"kind":"machine"}"#,
        r#""target":{"kind":"machine","name":"m","region":"x"}"#,
        r#""target":{"kind":"cluster","name":"m"}"#,
        r#""origin":{"kind":"interactive","interface":"mcp"}"#,
        r#""completion":{"checks":[{"kind":"command","id":"a","argv":["true"]}]}"#,
        r#""completion":{"checks":[{"kind":"command","id":"a","argv":["true"],"timeout_ms":1,"shell":true}]}"#,
        r#""completion":{"checks":[{"kind":"model_judgement","id":"a"}]}"#,
        r#""completion":{"profile":"p","mode":"replace"}"#,
        r#""tags":"alpha""#,
    ];
    for field in rejected {
        let json = format!("{{{base},{field}}}");
        assert!(decode(&json).is_err(), "{json}");
    }
    assert!(decode(&format!("{{{base}}}")).is_ok());
}

#[test]
fn validation_rejects_out_of_range_v2_options() {
    let digest = "6e4ddb1e2d01c811f83a059483e0853b2b74c59fd1e90327144c3e09c4ed5615";
    let check = |id: &str, path: &str, sha256: Option<&str>| CompletionCheck::File {
        id: id.into(),
        path: path.into(),
        sha256: sha256.map(Into::into),
    };
    let command = |argv: Vec<&str>, timeout_ms| CompletionCheck::Command {
        id: "c".into(),
        argv: argv.into_iter().map(Into::into).collect(),
        timeout_ms,
    };
    let criteria = |checks| {
        Some(CompletionCriteria {
            profile: None,
            checks,
        })
    };
    let mutations: Vec<Mutation> = vec![
        Box::new(|r| r.tags = vec![" ".into()]),
        Box::new(|r| r.tags = vec!["a".into(), "a".into()]),
        Box::new(|r| r.tags = (0..65).map(|n| n.to_string()).collect()),
        Box::new(|r| r.tags = vec!["x".repeat(65)]),
        Box::new(|r| r.title = Some(" ".into())),
        Box::new(|r| r.title = Some("é".repeat(201))),
        Box::new(|r| r.tool_profile = Some(String::new())),
        Box::new(|r| {
            r.target = Some(SessionTarget {
                kind: TargetKind::Machine,
                name: " ".into(),
            })
        }),
        Box::new(|r| {
            r.origin = Some(SessionOrigin::External {
                interface: Some(String::new()),
            })
        }),
        Box::new(|r| {
            r.workspace = Some(WorkspaceSelector::Repository {
                repository: "r".into(),
                checkout: Checkout::ChangeRequest(0),
            })
        }),
        Box::new(|r| {
            r.workspace = Some(WorkspaceSelector::Repository {
                repository: " ".into(),
                checkout: Checkout::Branch("b".into()),
            })
        }),
        Box::new(move |r| r.completion = criteria(vec![])),
        Box::new(move |r| {
            r.completion = criteria(vec![check("a", "x", None), check("a", "y", None)])
        }),
        Box::new(move |r| r.completion = criteria(vec![check("a", "../x", None)])),
        Box::new(move |r| r.completion = criteria(vec![check("a", "/etc/x", None)])),
        Box::new(move |r| r.completion = criteria(vec![check("a", "./x", None)])),
        Box::new(move |r| {
            r.completion = criteria(vec![check("a", "x", Some(&digest.to_uppercase()))])
        }),
        Box::new(move |r| r.completion = criteria(vec![check("a", "x", Some(&digest[1..]))])),
        Box::new(move |r| r.completion = criteria(vec![command(vec![], 1)])),
        Box::new(move |r| r.completion = criteria(vec![command(vec![" "], 1)])),
        Box::new(move |r| r.completion = criteria(vec![command(vec!["true"], 0)])),
        Box::new(move |r| r.completion = criteria(vec![command(vec!["true"], 600_001)])),
        Box::new(move |r| {
            r.completion = criteria((0..33).map(|n| check(&n.to_string(), "x", None)).collect())
        }),
    ];
    for (index, mutate) in mutations.iter().enumerate() {
        let mut request = legacy_create();
        mutate(&mut request);
        assert!(request.validate().is_err(), "mutation {index}");
    }
    let mut boundary = legacy_create();
    boundary.title = Some("é".repeat(200));
    boundary.tags = (0..64).map(|n| n.to_string()).collect();
    boundary.completion = criteria(vec![
        check("a", "dir/x", Some(digest)),
        command(vec!["true"], 600_000),
    ]);
    boundary.validate().unwrap();
}
