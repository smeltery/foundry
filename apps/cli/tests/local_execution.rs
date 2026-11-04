#![cfg(unix)]
use clap::Parser;
use f5y::{Backend, Request, args::Cli, dispatch, local::LocalBackend, runner::ExecutionBackend};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};
use tempfile::{TempDir, tempdir};

struct Fixture {
    _directory: TempDir,
    executable: PathBuf,
}
fn adapter() -> PathBuf {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let directory=tempdir().unwrap();
        let source=directory.path().join("fixture.rs");
        let executable=directory.path().join("fixture adapter");
        std::fs::write(&source,r#"
use std::io::{self, Write};
fn main() {
    println!("{{\"type\":\"ready\",\"version\":1}}");
    io::stdout().flush().unwrap();
    let mut input=String::new();io::stdin().read_line(&mut input).unwrap();
    let attempt=input.split("\"attempt_id\":\"").nth(1).unwrap().split('"').next().unwrap();
    if input.contains("__fixture_error__") {
        println!("{{\"type\":\"failed\",\"attempt_id\":\"{attempt}\",\"code\":\"test\",\"message\":\"test failure\"}}");
        return;
    }
    let outcome=if input.contains("__fixture_failure__") {"failure"}
        else if input.contains("__fixture_action__") {"action_required"} else {"success"};
    let cwd=std::env::current_dir().unwrap();
    let result=format!("cwd:{}",cwd.display());
    println!("{{\"type\":\"settled\",\"attempt_id\":\"{attempt}\",\"outcome\":\"{outcome}\",\"result\":{result:?}}}");
}
"#).unwrap();
        let output=Command::new("rustc").arg("--edition=2024").arg(&source).arg("-o").arg(&executable).output().unwrap();
        assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
        Fixture {_directory:directory,executable}
    }).executable.clone()
}
fn backend(path: &Path, configured: bool) -> ExecutionBackend<LocalBackend> {
    ExecutionBackend::new(path, LocalBackend::new(path), configured.then(adapter)).unwrap()
}
fn prompt(directory: &Path, text: &str) -> Request {
    Request {
        version: 1,
        command: json!({"operation":"prompt","arguments":{
            "prompt":text,"options":{"dir":directory,"yes":true}
        }}),
    }
}

#[test]
fn explicit_adapter_runs_literal_prompt_in_explicit_directory() {
    let directory = tempdir().unwrap();
    let db = directory.path().join("db");
    let marker = directory.path().join("must-not-exist");
    let task = format!("$(touch {}) ; __literal_task__", marker.display());
    let response = backend(&db, true)
        .execute(prompt(directory.path(), &task))
        .unwrap();
    // Unverified adapter success is action_required under foundry.completion.v1.
    assert_eq!(response.exit_code, 3);
    assert!(!marker.exists());
    assert_eq!(response.json["session"]["prompt"], task);
    let cwd = directory.path().canonicalize().unwrap();
    assert_eq!(response.text, format!("cwd:{}", cwd.display()));
    assert_eq!(response.json["execution"], "finished");
    let id = response.json["session"]["id"].as_str().unwrap();
    let stored = foundry_store::Store::open(&db).unwrap().get(id).unwrap();
    assert_eq!(stored.lifecycle, foundry_contracts::Lifecycle::Exit);
}

#[test]
fn outcomes_map_to_task_exit_codes() {
    // The fixture hard-codes its proposal, so success is never verified here.
    for (prompt_text, code) in [
        ("test", 3),
        ("__fixture_failure__", 1),
        ("__fixture_action__", 3),
    ] {
        let directory = tempdir().unwrap();
        let response = backend(&directory.path().join("db"), true)
            .execute(prompt(directory.path(), prompt_text))
            .unwrap();
        assert_eq!(response.exit_code, code);
    }
}

#[test]
fn runtime_failure_is_an_error_with_a_durable_error_session() {
    let directory = tempdir().unwrap();
    let db = directory.path().join("db");
    let error = backend(&db, true)
        .execute(prompt(directory.path(), "__fixture_error__"))
        .err()
        .unwrap();
    assert_eq!(error.code, "execution");
    assert!(error.message.contains("session "));
    let sessions = foundry_store::Store::open(&db).unwrap().list(10).unwrap();
    assert_eq!(sessions[0].lifecycle, foundry_contracts::Lifecycle::Error);
    assert!(sessions[0].outcome.is_none());
}

#[test]
fn unconfigured_prompt_fails_but_start_truthfully_queues() {
    let directory = tempdir().unwrap();
    let db = directory.path().join("db");
    let mut backend = backend(&db, false);
    let error = backend
        .execute(prompt(directory.path(), "test"))
        .err()
        .unwrap();
    assert_eq!(error.code, "adapter_not_configured");
    assert!(!db.exists());
    let queued = dispatch(
        Cli::parse_from(["f5y", "session", "start", "--prompt", "queued"]),
        &mut backend,
    )
    .unwrap();
    assert_eq!(queued.json["execution"], "not_started");
    assert_eq!(queued.json["session"]["lifecycle"], "queued");
}

#[test]
fn run_and_configured_start_use_the_service() {
    for argv in [
        vec!["f5y", "run", "--task", "test"],
        vec!["f5y", "session", "start", "--prompt", "test"],
    ] {
        let directory = tempdir().unwrap();
        let mut backend = backend(&directory.path().join("db"), true);
        let response = dispatch(Cli::parse_from(argv), &mut backend).unwrap();
        assert_eq!(response.json["execution"], "finished");
        // Unverified adapter success is action_required under foundry.completion.v1.
        assert_eq!(response.exit_code, 3);
    }
}

#[test]
fn rejects_active_unsupported_options_before_database_creation() {
    let directory = tempdir().unwrap();
    let db = directory.path().join("db");
    let mut backend = backend(&db, true);
    for (key, value) in [
        ("cloud", json!(true)),
        ("on", json!("worker")),
        ("repo", json!("a/b")),
        ("model", json!("model")),
        ("resume", json!("session")),
        ("detach", json!(true)),
        ("update", json!(true)),
        ("no_repo", json!(true)),
        ("unknown", Value::Null),
    ] {
        let mut request = prompt(directory.path(), "test");
        request.command["arguments"]["options"][key] = value;
        assert_eq!(
            backend.execute(request).err().unwrap().code,
            "unsupported",
            "{key}"
        );
    }
    for argv in [
        vec!["f5y", "run", "--task", "test", "--ref", "branch"],
        vec!["f5y", "run", "--task", "test", "--detach"],
        vec![
            "f5y",
            "session",
            "start",
            "--prompt",
            "test",
            "--provider",
            "github",
        ],
    ] {
        assert_eq!(
            dispatch(Cli::parse_from(argv), &mut backend)
                .err()
                .unwrap()
                .code,
            "unsupported"
        );
    }
    assert!(!db.exists());
}

#[test]
fn rejects_bad_paths_versions_shapes_and_empty_prompts() {
    let directory = tempdir().unwrap();
    let db = directory.path().join("db");
    assert!(
        ExecutionBackend::new(
            &db,
            LocalBackend::new(&db),
            Some(PathBuf::from("sh -c test"))
        )
        .is_err()
    );
    let mut backend = backend(&db, true);
    let mut request = prompt(directory.path(), "test");
    request.version = 2;
    assert_eq!(backend.execute(request).err().unwrap().code, "unsupported");
    assert_eq!(
        backend
            .execute(Request {
                version: 1,
                command: json!(null)
            })
            .err()
            .unwrap()
            .code,
        "invalid"
    );
    assert_eq!(
        backend
            .execute(prompt(directory.path(), " "))
            .err()
            .unwrap()
            .code,
        "invalid"
    );
    let request = prompt(&directory.path().join("missing"), "test");
    assert_eq!(backend.execute(request).err().unwrap().code, "invalid");
    assert!(!db.exists());
}

#[test]
fn read_commands_delegate_to_local_backend() {
    let directory = tempdir().unwrap();
    let db = directory.path().join("db");
    let mut backend = backend(&db, true);
    backend.execute(prompt(directory.path(), "test")).unwrap();
    let response = dispatch(Cli::parse_from(["f5y", "ls"]), &mut backend).unwrap();
    assert_eq!(response.json["sessions"].as_array().unwrap().len(), 1);
}

#[test]
fn exit_zero_and_mcp_success_come_only_from_a_verified_decision() {
    use foundry_contracts::{CompletionCheck, CompletionCriteria, CreateSession};
    use foundry_mcp::Backend as McpBackend;
    let notes = CompletionCheck::File {
        id: "notes".into(),
        path: "notes.txt".into(),
        sha256: None,
    };
    for (checks, code, outcome, cause) in [
        (vec![notes], 0, "success", "criteria_verified"),
        (Vec::new(), 3, "action_required", "criteria_unverified"),
    ] {
        let directory = tempdir().unwrap();
        std::fs::write(directory.path().join("notes.txt"), "notes").unwrap();
        let db = directory.path().join("db");
        let mut service = foundry_service::Service::open(&db).unwrap();
        let session = service
            .create(CreateSession {
                prompt: "test".into(),
                directory: directory.path().display().to_string(),
                completion: (!checks.is_empty()).then_some(CompletionCriteria {
                    profile: None,
                    checks,
                }),
                ..Default::default()
            })
            .unwrap();
        let mut backend = LocalBackend::new(&db);
        let queued = backend
            .call("session_get", json!({"session_id": session.id}))
            .unwrap();
        assert_eq!(queued["completion"]["policy_version"], Value::Null);
        let spec = foundry_runtime::AdapterSpec::new(adapter());
        service.run(&session.id, session.version, &spec).unwrap();
        let watched = Command::new(env!("CARGO_BIN_EXE_f5y"))
            .args(["session", "watch", &session.id, "--json", "--database"])
            .arg(&db)
            .env_remove("FOUNDRY_ADAPTER")
            .output()
            .unwrap();
        assert_eq!(watched.status.code(), Some(code));
        let got = backend
            .call("session_get", json!({"session_id": session.id}))
            .unwrap();
        assert_eq!(got["session"]["outcome"], outcome);
        assert_eq!(got["completion"]["policy_version"], "foundry.completion.v1");
        assert_eq!(got["completion"]["cause"], cause);
    }
}
