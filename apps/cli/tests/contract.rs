use clap::{CommandFactory, Parser};
use f5y::{Backend, CommandError, Request, Response, UnavailableBackend, args::Cli, execute};
use serde_json::json;

#[derive(Default)]
struct FakeBackend {
    requests: Vec<Request>,
}

impl Backend for FakeBackend {
    fn execute(&mut self, request: Request) -> Result<Response, CommandError> {
        self.requests.push(request);
        Ok(Response {
            text: "accepted".into(),
            json: json!({"id": "test-session"}),
            exit_code: 0,
        })
    }
}

#[test]
fn parser_definition_is_consistent() {
    Cli::command().debug_assert();
}

#[test]
fn documented_commands_parse() {
    for arguments in [
        vec!["f5y"],
        vec!["f5y", "task", "--cloud", "-d", "--model", "test"],
        vec!["f5y", "--resume", "s1"],
        vec!["f5y", "run", "--task", "task", "--ref", "branch", "--json"],
        vec![
            "f5y",
            "session",
            "start",
            "--prompt",
            "task",
            "--provider",
            "gitlab",
        ],
        vec![
            "f5y",
            "session",
            "list",
            "--status",
            "all",
            "--fields",
            "title,status",
        ],
        vec!["f5y", "session", "send", "s1", "--message", "continue"],
        vec!["f5y", "session", "share-link", "s1", "--expires-in", "60"],
        vec!["f5y", "session", "download", "s1", "report", "--force"],
        vec!["f5y", "watch", "s1", "--interval", "2"],
        vec!["f5y", "cancel", "s1", "--hard"],
        vec!["f5y", "network", "who"],
        vec!["f5y", "network", "team/agent", "question"],
        vec!["f5y", "network", "serve", "codex"],
        vec!["f5y", "plugins", "add", "test", "--client", "codex"],
        vec!["f5y", "config", "get", "model"],
    ] {
        Cli::try_parse_from(&arguments).unwrap_or_else(|error| panic!("{arguments:?}: {error}"));
    }
}

#[test]
fn invalid_combinations_are_rejected() {
    for arguments in [
        vec!["f5y", "task", "--cloud", "--on", "worker"],
        vec!["f5y", "task", "--repo", "a/b", "--no-repo"],
        vec!["f5y", "run", "--task", " "],
        vec!["f5y", "session", "list", "--limit", "0"],
        vec!["f5y", "network", "team/agent"],
    ] {
        assert!(Cli::try_parse_from(&arguments).is_err(), "{arguments:?}");
    }
}

#[test]
fn backend_receives_task_and_json_output_is_single_object() {
    let cli = Cli::parse_from([
        "f5y",
        "run",
        "--task",
        "test task",
        "--output-format",
        "json",
    ]);
    let mut backend = FakeBackend::default();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        execute(cli, &mut backend, &mut stdout, &mut stderr).unwrap(),
        0
    );
    assert_eq!(
        backend.requests[0].command["arguments"]["task"],
        "test task"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&stdout).unwrap(),
        json!({"id":"test-session"})
    );
    assert!(stderr.is_empty());
}

#[test]
fn unsupported_commands_fail_without_stdout_or_side_effects() {
    for arguments in [
        vec!["f5y", "status"],
        vec!["f5y", "config", "set", "model", "test"],
        vec!["f5y"],
    ] {
        let cli = Cli::parse_from(arguments);
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(
            execute(cli, &mut UnavailableBackend, &mut stdout, &mut stderr).unwrap(),
            2
        );
        assert!(stdout.is_empty());
        assert!(
            String::from_utf8(stderr)
                .unwrap()
                .contains("does not implement")
        );
    }
}

#[test]
fn json_errors_are_structured_on_stderr() {
    let cli = Cli::parse_from(["f5y", "status", "--json"]);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    assert_eq!(
        execute(cli, &mut UnavailableBackend, &mut stdout, &mut stderr).unwrap(),
        2
    );
    assert!(stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&stderr).unwrap()["error"]["code"],
        "unsupported"
    );
}

#[test]
fn local_detach_is_rejected_before_backend() {
    let cli = Cli::parse_from(["f5y", "task", "--detach"]);
    let mut backend = FakeBackend::default();
    assert!(f5y::dispatch(cli, &mut backend).is_err());
    assert!(backend.requests.is_empty());
}

#[test]
fn version_is_available_without_backend() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_f5y"))
        .args(["version", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["name"],
        "f5y"
    );
    let version = serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
    assert_eq!(version["version"], env!("FOUNDRY_VERSION"));
    let flag = std::process::Command::new(env!("CARGO_BIN_EXE_f5y"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(flag.status.success());
    assert_eq!(
        String::from_utf8(flag.stdout).unwrap().trim(),
        format!("f5y {}", env!("FOUNDRY_VERSION"))
    );
}
