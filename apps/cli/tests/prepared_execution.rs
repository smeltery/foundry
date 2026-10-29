#![cfg(unix)]
#[path = "prepared_execution/native_routes.rs"]
mod native_routes;
#[path = "prepared_execution/support.rs"]
mod support;
use foundry_contracts::{Lifecycle, Outcome, adapter_selection::AdapterSelection};
use foundry_store::Store;
use serde_json::{Value, json};
use support::*;

#[test]
fn configured_tasks_resolve_cli_over_file_without_provider_inference() {
    for route in [
        vec!["task"],
        vec!["run", "--task", "task"],
        vec!["session", "start", "--prompt", "task"],
    ] {
        for overrides in [
            vec![],
            vec!["--model", "other-provider/opaque:model"],
            vec!["--effort", "high"],
            vec!["--model", "other", "--effort", "xhigh"],
        ] {
            let h = Fixture::new("ok");
            let base: AdapterSelection =
                serde_json::from_value(h.value()["selection"].clone()).unwrap();
            let output = h.command().args(&route).args(&overrides).output().unwrap();
            let body = response(&output);
            assert_eq!(output.status.code(), Some(0));
            let received = h.received();
            let selected: AdapterSelection =
                serde_json::from_value(received["selection"].clone()).unwrap();
            let model = overrides
                .iter()
                .position(|v| *v == "--model")
                .map(|i| overrides[i + 1])
                .unwrap_or("file-model");
            let effort = overrides
                .iter()
                .position(|v| *v == "--effort")
                .map(|i| overrides[i + 1])
                .unwrap_or("medium");
            assert_eq!(
                body["session"]["model"],
                json!({"provider":"fixture-provider","model":model,"effort":effort})
            );
            assert_eq!(selected.model, model);
            assert_eq!(selected.effort, effort);
            assert_eq!(selected.provider, base.provider);
            assert!(selected.workspace_identity == base.workspace_identity);
            assert!(selected.resource_identity == base.resource_identity);
            assert!(selected.policy_identity == base.policy_identity);
            assert_eq!(selected.digest().unwrap().to_hex(), received["digest"]);
            assert_eq!(
                selected.digest().unwrap() == base.digest().unwrap(),
                overrides.is_empty()
            );
            let store = Store::open(&h.database).unwrap();
            let session = store.get(body["session"]["id"].as_str().unwrap()).unwrap();
            assert_eq!(serde_json::to_value(session).unwrap(), body["session"]);
            let events = serde_json::to_string(
                &store
                    .events(body["session"]["id"].as_str().unwrap(), 0, 100)
                    .unwrap(),
            )
            .unwrap();
            assert!(
                !events.contains("private-key-canary") && !events.contains("private-system-canary")
            );
        }
    }
}

#[test]
fn no_cli_grants_and_yes_return_pending_without_tool_effect() {
    for yes in [false, true] {
        let h = Fixture::new("pending");
        let mut command = h.command();
        command.arg("task");
        if yes {
            command.arg("--yes");
        }
        let output = command.output().unwrap();
        let body = response(&output);
        assert_eq!(output.status.code(), Some(3));
        let store = Store::open(&h.database).unwrap();
        let id = body["session"]["id"].as_str().unwrap();
        assert_eq!(
            store.get(id).unwrap().outcome,
            Some(Outcome::ActionRequired)
        );
        assert!(store.tool_call(id, "call").unwrap().is_none());
        assert!(!h.work.join("out.txt").exists());
    }
}

#[test]
fn invalid_prepared_inputs_never_open_storage_or_spawn() {
    for mutation in [
        "directory",
        "agent",
        "credential",
        "grant",
        "model",
        "helper",
        "adapter",
        "file",
    ] {
        let h = Fixture::new("ok");
        let mut value = h.value();
        match mutation {
            "directory" => value["selection"]["directory"] = json!(h.root.path()),
            "agent" => value["private"]["agent_directory"] = json!(h.root.path().join("missing")),
            "credential" => value["private"]["credential"]["provider"] = json!("wrong"),
            "grant" => value["selection"]["tools"] = json!(["read"]),
            _ => {}
        }
        h.save(value);
        let mut command = h.command();
        command.arg("task");
        match mutation {
            "grant" => {
                command.args(["--tool-grant", "write.create"]);
            }
            "model" => {
                command.args(["--model", &"x".repeat(257)]);
            }
            "helper" => {
                command.env("FOUNDRY_TOOLHOST", "relative");
            }
            "adapter" => {
                command.env("FOUNDRY_ADAPTER", h.root.path().join("missing"));
            }
            "file" => {
                command.env("FOUNDRY_ADAPTER_CONFIG", "relative");
            }
            _ => {}
        }
        h.assert_preflight(&command.output().unwrap());
    }
}

#[test]
fn repository_provider_and_nonlocal_options_remain_unsupported() {
    for args in [
        vec![
            "session",
            "start",
            "--prompt",
            "task",
            "--provider",
            "github",
        ],
        vec!["task", "--cloud"],
        vec!["task", "--resume", "id"],
        vec!["run", "--task", "task", "--ref", "branch"],
    ] {
        let h = Fixture::new("ok");
        h.assert_preflight(&h.command().args(args).output().unwrap());
    }
}

#[test]
fn explicit_configuration_without_adapter_never_downgrades_to_queue() {
    let h = Fixture::new("ok");
    let output = h
        .command()
        .env_remove("FOUNDRY_ADAPTER")
        .args(["session", "start", "--prompt", "task"])
        .output()
        .unwrap();
    h.assert_preflight(&output);
}

#[test]
fn configuration_is_lazy_for_help_version_and_local_reads() {
    for args in [vec!["--help"], vec!["version"], vec!["ls"]] {
        let h = Fixture::new("ok");
        let output = h
            .command()
            .env("FOUNDRY_ADAPTER_CONFIG", h.root.path().join("missing"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        assert!(!h.work.join("received.json").exists());
        private_output(&output);
    }
}

#[test]
fn rejected_bootstrap_is_durable_error_without_start_or_private_diagnostics() {
    for mode in ["wrong_ack", "reject"] {
        let h = Fixture::new(mode);
        let output = h.command().arg("task").output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        private_output(&output);
        assert!(!h.work.join("started").exists());
        let store = Store::open(&h.database).unwrap();
        let sessions = store.list(10).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].lifecycle, Lifecycle::Error);
    }
}

#[test]
#[ignore = "requires freshly built f5y-toolhost via FOUNDRY_TEST_TOOLHOST"]
fn configured_explicit_grant_uses_real_helper_and_durable_receipt() {
    let h = Fixture::new("write");
    let helper = std::env::var_os("FOUNDRY_TEST_TOOLHOST").unwrap();
    let output = h
        .command()
        .env("FOUNDRY_TOOLHOST", helper)
        .args(["task", "--tool-grant", "write.create"])
        .output()
        .unwrap();
    let body = response(&output);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(h.work.join("out.txt")).unwrap(),
        "owned output"
    );
    let record = Store::open(&h.database)
        .unwrap()
        .tool_call(body["session"]["id"].as_str().unwrap(), "call")
        .unwrap()
        .unwrap();
    assert_eq!(record.outcome, Some(foundry_store::ToolOutcome::Success));
    assert!(record.output_digest.is_some());
}

#[test]
fn each_fresh_attempt_resolves_again_without_mutating_prior_session() {
    let h = Fixture::new("ok");
    let mut config = h.value();
    config["selection"]["effort"] = json!("max");
    h.save(config);
    let first = response(&h.command().arg("task").output().unwrap());
    assert_eq!(first["session"]["model"]["effort"], "max");
    let first_digest = h.received()["digest"].clone();
    let second = response(
        &h.command()
            .args(["task", "--model", "new:model", "--effort", "low"])
            .output()
            .unwrap(),
    );
    assert_eq!(second["session"]["model"]["model"], "new:model");
    assert_ne!(first["session"]["id"], second["session"]["id"]);
    assert_ne!(first_digest, h.received()["digest"]);
    let store = Store::open(&h.database).unwrap();
    let earlier = store.get(first["session"]["id"].as_str().unwrap()).unwrap();
    assert_eq!(serde_json::to_value(earlier).unwrap(), first["session"]);
    assert_eq!(h.value()["selection"]["model"], "file-model");
    assert_eq!(h.value()["selection"]["effort"], "max");
}

#[test]
fn malformed_direct_task_options_fail_without_database_or_process() {
    use f5y::{Backend, Request, local::LocalBackend, runner::ExecutionBackend};
    for (field, value) in [
        ("model", json!(42)),
        ("effort", json!(false)),
        ("effort", json!("off")),
        ("model", json!("")),
        ("unknown", Value::Null),
    ] {
        let h = Fixture::new("ok");
        let mut backend = ExecutionBackend::new(
            &h.database,
            LocalBackend::new(&h.database),
            Some(h.adapter.clone()),
        )
        .unwrap()
        .with_adapter_configuration(h.config.clone())
        .with_toolhost("/usr/bin/true".into());
        let mut command =
            json!({"operation":"prompt","arguments":{"prompt":"task","options":{"dir":h.work}}});
        command["arguments"]["options"][field] = value;
        assert!(
            backend
                .execute(Request {
                    version: 1,
                    command
                })
                .is_err()
        );
        assert!(!h.database.exists());
        assert!(!h.work.join("received.json").exists());
    }
}
