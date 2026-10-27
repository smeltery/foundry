#![cfg(unix)]
#[path = "support/tools.rs"]
mod support;
use foundry_contracts::Lifecycle;
use foundry_service::ServiceOptions;
use foundry_store::{Store, ToolOutcome};
use foundry_tools::host::Grant;
use std::sync::{Arc, atomic::AtomicU8};
use support::Fixture;

fn run(
    fixture: &mut Fixture,
    adapter: &str,
    grants: Vec<Grant>,
) -> foundry_service::Result<foundry_contracts::Session> {
    let policy = fixture.policy(grants);
    let spec = fixture.adapter(adapter);
    fixture.service.run_with_tools(
        &fixture.session,
        0,
        &spec,
        Arc::new(AtomicU8::new(0)),
        policy,
    )
}

#[test]
fn tool_result_follows_durable_receipt_and_survives_reopen() {
    for (mode, outcome) in [
        ("success", ToolOutcome::Success),
        ("denied", ToolOutcome::Failed),
    ] {
        let mut fixture = Fixture::new(mode, ServiceOptions::default());
        let result = run(&mut fixture, "read", vec![Grant::Read]).unwrap();
        assert_eq!(result.lifecycle, Lifecycle::Exit);
        let record = Store::open(&fixture.database)
            .unwrap()
            .tool_call(&fixture.session, "call")
            .unwrap()
            .unwrap();
        assert_eq!(record.outcome, Some(outcome));
        assert!(record.output_digest.is_some());
        assert!(fixture.directory.path().join("observed").exists());
    }
}

#[test]
fn unknown_effects_and_bad_completion_never_reach_adapter_as_success() {
    for mode in [
        "uncertain",
        "wrong",
        "missing",
        "oversize",
        "bad_exit",
        "effect_then_die",
    ] {
        let mut fixture = Fixture::new(mode, ServiceOptions::default());
        assert!(
            run(&mut fixture, "read", vec![Grant::Read]).is_err(),
            "{mode}"
        );
        let store = Store::open(&fixture.database).unwrap();
        let record = store.tool_call(&fixture.session, "call").unwrap().unwrap();
        assert_eq!(record.outcome, Some(ToolOutcome::Uncertain), "{mode}");
        assert_eq!(
            store.get(&fixture.session).unwrap().lifecycle,
            Lifecycle::Error
        );
        assert!(!fixture.directory.path().join("observed").exists());
        if mode == "effect_then_die" {
            assert!(fixture.directory.path().join("effect").exists());
        }
    }
}

#[test]
fn failed_claim_never_spawns_helper_and_failed_receipt_never_delivers_result() {
    for claim in [true, false] {
        let mut fixture = Fixture::new("success", ServiceOptions::default());
        let connection = rusqlite::Connection::open(&fixture.database).unwrap();
        connection.execute_batch(if claim {
            "CREATE TRIGGER reject_tool BEFORE INSERT ON tool_calls BEGIN SELECT RAISE(FAIL,'offline claim failure'); END;"
        } else {
            "CREATE TRIGGER reject_tool BEFORE UPDATE ON tool_calls BEGIN SELECT RAISE(FAIL,'offline receipt failure'); END;"
        }).unwrap();
        assert!(run(&mut fixture, "read", vec![Grant::Read]).is_err());
        let record = Store::open(&fixture.database)
            .unwrap()
            .tool_call(&fixture.session, "call")
            .unwrap();
        if claim {
            assert!(record.is_none());
            assert!(!fixture.directory.path().join("helper_started").exists());
        } else {
            assert!(record.unwrap().outcome.is_none());
        }
        assert!(!fixture.directory.path().join("observed").exists());
    }
}

#[test]
fn default_deny_and_injected_authority_never_spawn_helpers() {
    for injected in [false, true] {
        let mut fixture = Fixture::new("success", ServiceOptions::default());
        let result = run(
            &mut fixture,
            if injected { "injected" } else { "read" },
            if injected { vec![Grant::Read] } else { vec![] },
        )
        .unwrap();
        assert_eq!(result.lifecycle, Lifecycle::Exit);
        assert!(!fixture.directory.path().join("helper_started").exists());
        assert!(
            Store::open(&fixture.database)
                .unwrap()
                .tool_call(&fixture.session, "call")
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn repeated_call_in_a_later_attempt_is_not_reexecuted() {
    let mut fixture = Fixture::new("success", ServiceOptions::default());
    let completed = run(&mut fixture, "read", vec![Grant::Read]).unwrap();
    std::fs::remove_file(fixture.directory.path().join("helper_started")).unwrap();
    let policy = fixture.policy(vec![Grant::Read]);
    let adapter = fixture.adapter("read");
    assert!(
        fixture
            .service
            .run_with_tools(
                &fixture.session,
                completed.version,
                &adapter,
                Arc::new(AtomicU8::new(0)),
                policy
            )
            .is_err()
    );
    assert!(!fixture.directory.path().join("helper_started").exists());
    assert_eq!(
        Store::open(&fixture.database)
            .unwrap()
            .tool_call(&fixture.session, "call")
            .unwrap()
            .unwrap()
            .outcome,
        Some(ToolOutcome::Success)
    );
}

#[test]
fn duplicate_and_premature_settlement_are_errors() {
    for mode in ["duplicate", "premature"] {
        let mut fixture = Fixture::new("success", ServiceOptions::default());
        assert!(run(&mut fixture, mode, vec![Grant::Read]).is_err());
        assert!(
            fixture
                .service
                .get(&fixture.session)
                .unwrap()
                .outcome
                .is_none()
        );
    }
}

#[test]
#[ignore = "requires freshly built f5y-toolhost via FOUNDRY_TEST_TOOLHOST"]
fn real_toolhost_executes_a_granted_write_and_commits_receipt() {
    let helper = std::path::PathBuf::from(
        std::env::var_os("FOUNDRY_TEST_TOOLHOST")
            .expect("build f5y-toolhost and set FOUNDRY_TEST_TOOLHOST to its absolute path"),
    );
    let mut fixture = Fixture::new("unused", ServiceOptions::default());
    let policy = support::policy(&helper, fixture.directory.path(), vec![Grant::Create]);
    let adapter = fixture.adapter("write");
    let result = fixture
        .service
        .run_with_tools(
            &fixture.session,
            0,
            &adapter,
            Arc::new(AtomicU8::new(0)),
            policy,
        )
        .unwrap();
    assert_eq!(result.lifecycle, Lifecycle::Exit);
    assert_eq!(
        std::fs::read_to_string(fixture.directory.path().join("output.txt")).unwrap(),
        "offline content"
    );
    assert_eq!(
        Store::open(&fixture.database)
            .unwrap()
            .tool_call(&fixture.session, "call")
            .unwrap()
            .unwrap()
            .outcome,
        Some(ToolOutcome::Success)
    );
}

#[path = "support/tool_cancellation.rs"]
mod cancellation;
