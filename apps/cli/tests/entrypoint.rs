use serde_json::{Value, json};
use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};
use tempfile::TempDir;

fn command(directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_f5y"));
    command
        .current_dir(directory)
        .env_remove("FOUNDRY_DATABASE")
        .env_remove("FOUNDRY_ADAPTER");
    command
}
fn run(directory: &Path, args: &[&str]) -> Output {
    command(directory).args(args).output().unwrap()
}
fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn persistent_sessions_across_separate_processes() {
    let directory = TempDir::new().unwrap();
    let path = directory.path();
    let created = success(run(
        path,
        &[
            "session",
            "start",
            "--prompt",
            "fixture",
            "--database",
            "state.db",
            "--json",
        ],
    ));
    assert_eq!(created["execution"], "not_started");
    assert_eq!(created["target"], "local");
    let id = created["session"]["id"].as_str().unwrap();
    let listed = success(run(path, &["ls", "--database", "state.db", "--json"]));
    assert_eq!(listed["sessions"][0]["id"], id);
    let fetched = success(run(
        path,
        &["session", "get", id, "--database", "state.db", "--json"],
    ));
    assert_eq!(fetched["session"]["lifecycle"], "queued");
    let cancelled = success(run(
        path,
        &["cancel", id, "--hard", "--database", "state.db", "--json"],
    ));
    assert_eq!(cancelled["cancellation"], "requested");
    assert_eq!(cancelled["session"]["lifecycle"], "suspended");
    let fetched = success(run(
        path,
        &["session", "get", id, "--database", "state.db", "--json"],
    ));
    assert_eq!(fetched["session"]["lifecycle"], "suspended");
}

#[test]
fn configuration_is_required_but_help_and_version_are_exempt() {
    let directory = TempDir::new().unwrap();
    for args in [&["ls"][..], &["mcp"], &["ls", "--json"]] {
        let output = run(directory.path(), args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("--database PATH"));
        assert!(stderr.contains("FOUNDRY_DATABASE"));
    }
    for args in [
        &["--help"][..],
        &["--version"],
        &["version"],
        &["version", "--json"],
    ] {
        let output = run(directory.path(), args);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn explicit_database_overrides_environment_and_empty_is_rejected() {
    let directory = TempDir::new().unwrap();
    let path = directory.path();
    let output = command(path)
        .env("FOUNDRY_DATABASE", "environment.db")
        .args(["ls", "--database", "explicit.db", "--json"])
        .output()
        .unwrap();
    success(output);
    assert!(path.join("explicit.db").exists());
    assert!(!path.join("environment.db").exists());
    success(
        command(path)
            .env("FOUNDRY_DATABASE", "environment.db")
            .args(["ls", "--json"])
            .output()
            .unwrap(),
    );
    assert!(path.join("environment.db").exists());
    let output = command(path)
        .env("FOUNDRY_DATABASE", "")
        .args(["ls", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "configuration");
}

#[test]
fn watch_requires_existing_database_and_does_not_create_it() {
    let directory = TempDir::new().unwrap();
    for args in [
        vec!["watch", "missing", "--database", "state.db", "--json"],
        vec![
            "session",
            "watch",
            "missing",
            "--database",
            "state.db",
            "--json",
        ],
    ] {
        let output = run(directory.path(), &args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "watch");
        assert!(!directory.path().join("state.db").exists());
    }
}

#[test]
fn mcp_stdio_has_only_protocol_frames_and_persists_created_session() {
    let directory = TempDir::new().unwrap();
    let mut child = command(directory.path())
        .env("FOUNDRY_ADAPTER", "invalid-relative-path")
        .args(["mcp", "--database", "state.db"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let messages = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":foundry_mcp::PROTOCOL_VERSION,"capabilities":{},
            "clientInfo":{"name":"fixture","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"session_create","arguments":{"prompt":"fixture","directory":"/tmp"}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"session_list"}}),
    ];
    {
        let mut stdin = child.stdin.take().unwrap();
        for message in messages {
            writeln!(stdin, "{message}").unwrap();
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let frames: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(frames.len(), 3);
    for (frame, id) in frames.iter().zip([1, 2, 3]) {
        assert_eq!(frame["jsonrpc"], "2.0");
        assert_eq!(frame["id"], id);
        assert!(frame.get("error").is_none());
    }
    let created: Value =
        serde_json::from_str(frames[1]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(created["execution"], "not_started");
    let listed: Value =
        serde_json::from_str(frames[2]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(listed["sessions"][0]["id"], created["session"]["id"]);
    let persisted = success(run(
        directory.path(),
        &["ls", "--database", "state.db", "--json"],
    ));
    assert_eq!(persisted["sessions"][0]["id"], created["session"]["id"]);
}

#[test]
fn database_global_option_works_before_subcommand() {
    let directory = TempDir::new().unwrap();
    let result = success(run(
        directory.path(),
        &["--database", "state.db", "ls", "--json"],
    ));
    assert_eq!(result["sessions"], json!([]));
}

#[test]
fn unconfigured_prompt_and_adapter_errors_do_not_create_database() {
    let directory = TempDir::new().unwrap();
    let path = directory.path();
    let output = run(path, &["fixture", "--database", "state.db", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "adapter_not_configured");
    assert!(!path.join("state.db").exists());
    for format in [
        vec!["ls", "--database", "state.db", "--json"],
        vec!["ls", "--database", "state.db"],
    ] {
        let output = command(path)
            .env("FOUNDRY_ADAPTER", "relative-path")
            .args(format.clone())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        if format.contains(&"--json") {
            let error: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(error["error"]["code"], "invalid");
        } else {
            assert!(
                String::from_utf8(output.stderr)
                    .unwrap()
                    .starts_with("error:")
            );
        }
        assert!(!path.join("state.db").exists());
    }
    let version = command(path)
        .env("FOUNDRY_ADAPTER", "relative-path")
        .args(["version"])
        .output()
        .unwrap();
    assert!(version.status.success());
    assert!(version.stderr.is_empty());
}

#[test]
fn root_task_options_cannot_be_silently_ignored_by_subcommands() {
    let directory = TempDir::new().unwrap();
    let output = run(
        directory.path(),
        &["--cloud", "--database", "state.db", "ls", "--json"],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "usage");
    assert!(!directory.path().join("state.db").exists());
}

#[test]
fn watch_live_task_and_finished_session_with_both_command_forms() {
    use std::time::{Duration, Instant};
    let directory = TempDir::new().unwrap();
    let path = directory.path();
    let source = path.join("fixture.rs");
    let executable = path.join("fixture-adapter");
    std::fs::write(&source, r#"
use std::{io::{self, Write}, time::Duration};
fn main() {
    println!("{{\"type\":\"ready\",\"version\":1}}");
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    let attempt = input.split("\"attempt_id\":\"").nth(1).unwrap().split('"').next().unwrap();
    let id = input.split("\"id\":\"").nth(1).unwrap().split('"').next().unwrap();
    println!("{{\"type\":\"text\",\"attempt_id\":\"{attempt}\",\"text\":\"progress\"}}");
    io::stdout().flush().unwrap();
    std::fs::write("session-id", id).unwrap();
    for _ in 0..1000 {
        if std::path::Path::new("release").exists() { break; }
        std::thread::sleep(Duration::from_millis(10));
    }
    println!("{{\"type\":\"settled\",\"attempt_id\":\"{attempt}\",\"outcome\":\"success\",\"result\":\"done\"}}");
}
"#).unwrap();
    let compiled = Command::new("rustc")
        .args(["--edition=2024"])
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let mut task = command(path)
        .env("FOUNDRY_ADAPTER", &executable)
        .args(["fixture", "--database", "state.db", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let id = loop {
        if let Ok(id) = std::fs::read_to_string(path.join("session-id")) {
            break id;
        }
        if Instant::now() >= deadline || task.try_wait().unwrap().is_some() {
            let _ = task.kill();
            let output = task.wait_with_output().unwrap();
            panic!(
                "task did not start: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut watcher = command(path)
        .env("FOUNDRY_ADAPTER", "invalid-relative-path")
        .args([
            "watch",
            &id,
            "--interval",
            "1",
            "--database",
            "state.db",
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let waiting = watcher.try_wait().unwrap().is_none();
    std::fs::write(path.join("release"), "continue").unwrap();
    assert!(waiting, "watch must wait for the running task");
    while task.try_wait().unwrap().is_none() || watcher.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = task.kill();
            let _ = watcher.kill();
            let _ = task.wait();
            let _ = watcher.wait();
            panic!("task or watcher exceeded deadline");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        success(task.wait_with_output().unwrap())["execution"],
        "finished"
    );
    let replay = run(
        path,
        &["session", "watch", &id, "--database", "state.db", "--json"],
    );
    for output in [watcher.wait_with_output().unwrap(), replay] {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let lines: Vec<Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(lines.iter().any(|line| line["type"] == "event"));
        let result = lines.last().unwrap();
        assert_eq!(result["type"], "result");
        assert_eq!(result["session"]["result"], "done");
        assert_eq!(result["exit_code"], 0);
    }
    let output = run(
        path,
        &["watch", "missing", "--database", "state.db", "--json"],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "watch");
}
