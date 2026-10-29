#![cfg(unix)]
#[allow(dead_code)]
#[path = "support/tools.rs"]
mod support;
use foundry_contracts::{
    Lifecycle, Outcome,
    permission::{PendingMode, PermissionDecision, PermissionProfile},
};
use foundry_service::{
    ServiceOptions,
    permission::{PermissionTools, ToolPermissionSettings, ToolPermissions},
};
use foundry_store::{Store, ToolOutcome};
use foundry_tools::host::Grant;
use std::{
    sync::{Arc, atomic::AtomicU8},
    time::Duration,
};
use support::Fixture;

fn selected(
    f: &Fixture,
    grants: Vec<Grant>,
    missing: PermissionDecision,
    pending: PendingMode,
) -> PermissionTools {
    let policy = f.policy(vec![
        Grant::Read,
        Grant::List,
        Grant::Create,
        Grant::Overwrite,
    ]);
    let permissions = ToolPermissions::new(
        policy.identity(),
        ToolPermissionSettings {
            profile: PermissionProfile::Current,
            configuration_digest: [7; 32],
            generation: 1,
            grants,
            missing_grant: missing,
            pending_mode: pending,
            request_timeout: Duration::from_secs(3),
        },
    )
    .unwrap();
    PermissionTools::new(policy, permissions).unwrap()
}
fn run(
    f: &mut Fixture,
    tools: PermissionTools,
    adapter: &str,
) -> foundry_service::Result<foundry_contracts::Session> {
    let spec = f.adapter(adapter);
    f.service
        .run_with_permissions(&f.session, 0, &spec, Arc::new(AtomicU8::new(0)), tools)
}

#[test]
fn allowed_call_consumes_permission_and_delivers_only_after_durable_receipt() {
    let mut f = Fixture::new("success", ServiceOptions::default());
    let tools = selected(
        &f,
        vec![Grant::Read],
        PermissionDecision::Pending,
        PendingMode::ActionRequired,
    );
    assert_eq!(tools.configuration_digest(), [7; 32]);
    assert_eq!(
        run(&mut f, tools, "read").unwrap().outcome,
        Some(Outcome::Success)
    );
    let record = Store::open(&f.database)
        .unwrap()
        .tool_call(&f.session, "call")
        .unwrap()
        .unwrap();
    assert_eq!(record.outcome, Some(ToolOutcome::Success));
    assert!(record.output_digest.is_some());
    assert!(f.directory.path().join("helper_started").exists());
    assert!(f.directory.path().join("observed").exists());
}

#[test]
fn explicit_denial_retains_denied_delivery_without_a_helper_claim() {
    let mut f = Fixture::new("success", ServiceOptions::default());
    let tools = selected(
        &f,
        vec![],
        PermissionDecision::Deny,
        PendingMode::ActionRequired,
    );
    assert_eq!(
        run(&mut f, tools, "read").unwrap().outcome,
        Some(Outcome::Success)
    );
    let observed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(f.directory.path().join("observed")).unwrap())
            .unwrap();
    assert_eq!(observed["result"]["status"], "denied");
    assert!(observed["outcome"].is_null());
    assert!(!f.directory.path().join("helper_started").exists());
    assert!(
        Store::open(&f.database)
            .unwrap()
            .tool_call(&f.session, "call")
            .unwrap()
            .is_none()
    );
}

#[test]
fn pending_and_unsupported_never_become_a_fake_denial_or_receipt() {
    for (decision, mode, action_required) in [
        (
            PermissionDecision::Pending,
            PendingMode::ActionRequired,
            true,
        ),
        (PermissionDecision::Pending, PendingMode::Unsupported, false),
        (
            PermissionDecision::Unsupported,
            PendingMode::ActionRequired,
            false,
        ),
    ] {
        let mut f = Fixture::new("success", ServiceOptions::default());
        let tools = selected(&f, vec![], decision, mode);
        let result = run(&mut f, tools, "read");
        if action_required {
            let session = result.unwrap();
            assert_eq!(session.lifecycle, Lifecycle::Exit);
            assert_eq!(session.outcome, Some(Outcome::ActionRequired));
            assert_eq!(
                session.result.as_deref(),
                Some("permission_action_required")
            );
        } else {
            assert!(matches!(
                result,
                Err(foundry_service::ServiceError::Runtime(
                    foundry_runtime::RuntimeError::Protocol("permission_unsupported")
                ))
            ));
        }
        let store = Store::open(&f.database).unwrap();
        assert!(store.tool_call(&f.session, "call").unwrap().is_none());
        assert!(store.get(&f.session).unwrap().active_attempt_id.is_none());
        assert!(!f.directory.path().join("helper_started").exists());
        assert!(!f.directory.path().join("observed").exists());
        assert!(!support::alive(&f.directory.path().join("adapter_started")));
    }
}

#[test]
fn claim_failure_and_duplicate_requests_never_reissue_permission() {
    for duplicate in [false, true] {
        let mut f = Fixture::new("success", ServiceOptions::default());
        if !duplicate {
            rusqlite::Connection::open(&f.database).unwrap().execute_batch(
                "CREATE TRIGGER reject_tool BEFORE INSERT ON tool_calls BEGIN SELECT RAISE(FAIL,'claim rejected'); END;"
            ).unwrap();
        }
        let tools = selected(
            &f,
            vec![Grant::Read],
            PermissionDecision::Pending,
            PendingMode::ActionRequired,
        );
        assert!(run(&mut f, tools, if duplicate { "duplicate" } else { "read" }).is_err());
        let store = Store::open(&f.database).unwrap();
        let record = store.tool_call(&f.session, "call").unwrap();
        if duplicate {
            assert_eq!(record.unwrap().outcome, Some(ToolOutcome::Success));
        } else {
            assert!(record.is_none());
            assert!(!f.directory.path().join("helper_started").exists());
        }
    }
}

#[test]
fn consumed_permission_does_not_hide_uncertain_effect_or_failed_receipt() {
    for mode in ["effect_then_die", "receipt_failure"] {
        let mut f = Fixture::new(
            if mode == "receipt_failure" {
                "success"
            } else {
                mode
            },
            ServiceOptions::default(),
        );
        if mode == "receipt_failure" {
            rusqlite::Connection::open(&f.database).unwrap().execute_batch(
                "CREATE TRIGGER reject_receipt BEFORE UPDATE ON tool_calls BEGIN SELECT RAISE(FAIL,'receipt rejected'); END;"
            ).unwrap();
        }
        let tools = selected(
            &f,
            vec![Grant::Read],
            PermissionDecision::Pending,
            PendingMode::ActionRequired,
        );
        assert!(run(&mut f, tools, "read").is_err());
        let record = Store::open(&f.database)
            .unwrap()
            .tool_call(&f.session, "call")
            .unwrap()
            .unwrap();
        assert_eq!(
            record.outcome,
            if mode == "receipt_failure" {
                None
            } else {
                Some(ToolOutcome::Uncertain)
            }
        );
        assert!(!f.directory.path().join("observed").exists());
    }
}

#[test]
#[ignore = "requires freshly built f5y-toolhost via FOUNDRY_TEST_TOOLHOST"]
fn real_toolhost_creation_consumes_permission_once() {
    let helper = std::path::PathBuf::from(
        std::env::var_os("FOUNDRY_TEST_TOOLHOST").expect("explicit helper required"),
    );
    let mut f = Fixture::new("unused", ServiceOptions::default());
    let policy = support::policy(&helper, f.directory.path(), vec![Grant::Create]);
    let permissions = ToolPermissions::new(
        policy.identity(),
        ToolPermissionSettings {
            profile: PermissionProfile::Legacy,
            configuration_digest: [8; 32],
            generation: 1,
            grants: vec![Grant::Create],
            missing_grant: PermissionDecision::Pending,
            pending_mode: PendingMode::ActionRequired,
            request_timeout: Duration::from_secs(3),
        },
    )
    .unwrap();
    let tools = PermissionTools::new(policy, permissions).unwrap();
    assert_eq!(
        run(&mut f, tools, "write").unwrap().outcome,
        Some(Outcome::Success)
    );
    assert_eq!(
        std::fs::read_to_string(f.directory.path().join("output.txt")).unwrap(),
        "offline content"
    );
    assert_eq!(
        Store::open(&f.database)
            .unwrap()
            .tool_call(&f.session, "call")
            .unwrap()
            .unwrap()
            .outcome,
        Some(ToolOutcome::Success)
    );
}
