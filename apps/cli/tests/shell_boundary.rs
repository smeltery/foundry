use clap::Parser;
use f5y::{
    Backend, CommandError, Request, Response, UnavailableBackend, args::Cli, dispatch, execute,
};
use serde_json::{Value, json};
use std::process::{Command, Output};
use tempfile::TempDir;

#[derive(Default)]
struct Recorder {
    requests: Vec<Request>,
    status: u8,
}
impl Backend for Recorder {
    fn execute(&mut self, request: Request) -> Result<Response, CommandError> {
        self.requests.push(request);
        Ok(Response {
            text: "backend result".into(),
            json: json!({"result":"backend result"}),
            exit_code: self.status,
        })
    }
}
fn parse(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("f5y").chain(args.iter().copied())).unwrap()
}
fn binary(dir: &TempDir, args: &[&str], configured: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_f5y"));
    command
        .current_dir(dir.path())
        .args(args)
        .env_remove("FOUNDRY_DATABASE")
        .env("FOUNDRY_ADAPTER", "invalid-relative-adapter");
    if configured {
        command.args(["--database", "state.db"]);
    }
    command.output().unwrap()
}

#[test]
fn root_task_conflicts_match_fake_and_binary_before_configuration() {
    for args in [
        vec!["--cloud", "ls"],
        vec!["--model", "fixture", "status"],
        vec!["--dir", ".", "session", "list"],
        vec!["--detach", "watch", "session-id"],
        vec!["--resume", "session-id", "mcp"],
        vec!["--yes", "update"],
        vec!["--on", "worker", "connect"],
        vec!["--update", "ls"],
    ] {
        for json in [false, true] {
            let mut args = args.clone();
            if json {
                args.push("--json");
            }
            let mut backend = Recorder::default();
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            assert_eq!(
                execute(parse(&args), &mut backend, &mut stdout, &mut stderr).unwrap(),
                2
            );
            assert!(stdout.is_empty());
            assert!(backend.requests.is_empty());
            for configured in [false, true] {
                let dir = TempDir::new().unwrap();
                let output = binary(&dir, &args, configured);
                assert_eq!(output.status.code(), Some(2), "{args:?}");
                assert_eq!(output.stdout, stdout);
                assert_eq!(output.stderr, stderr, "{args:?}");
                assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
            }
        }
    }
}

#[test]
fn invalid_root_tasks_are_validated_before_configuration_too() {
    for args in [vec!["--detach", "task"], vec![" \n\t"]] {
        let mut backend = Recorder::default();
        let error = dispatch(parse(&args), &mut backend).err().unwrap();
        assert_eq!(error.code, "usage");
        assert!(backend.requests.is_empty());
        let mut args = args;
        args.push("--json");
        for configured in [false, true] {
            let dir = TempDir::new().unwrap();
            let output = binary(&dir, &args, configured);
            assert_eq!(output.status.code(), Some(2));
            assert!(output.stdout.is_empty());
            let value: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(value["error"]["code"], error.code);
            assert_eq!(value["error"]["message"], error.message);
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        }
    }
}

#[test]
fn help_version_and_normalization_preserve_existing_exceptions() {
    for args in [
        vec!["--cloud", "help", "session"],
        vec!["--cloud", "version"],
    ] {
        let mut backend = Recorder::default();
        assert_eq!(dispatch(parse(&args), &mut backend).unwrap().exit_code, 0);
        assert!(backend.requests.is_empty());
        let dir = TempDir::new().unwrap();
        let output = binary(&dir, &args, true);
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(!output.stdout.is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
    for args in [vec!["--upgrade"], vec!["upgrade"], vec!["update"]] {
        let mut cli = parse(&args);
        f5y::normalize_and_validate(&mut cli).unwrap();
        let mut backend = Recorder::default();
        dispatch(cli, &mut backend).unwrap();
        assert_eq!(backend.requests.len(), 1);
        assert_eq!(backend.requests[0].command["operation"], "update");
    }
    for args in [vec!["--upgrade", "--cloud"], vec!["--upgrade", "version"]] {
        let mut backend = Recorder::default();
        assert_eq!(
            dispatch(parse(&args), &mut backend).err().unwrap().code,
            "usage"
        );
        assert!(backend.requests.is_empty());
    }
}

#[test]
fn valid_task_and_subcommand_options_survive_shared_validation() {
    for args in [
        vec!["--cloud", "--detach", "task", "--tool-grant", "read"],
        vec!["run", "--task", "task", "--detach"],
    ] {
        let mut cli = parse(&args);
        f5y::normalize_and_validate(&mut cli).unwrap();
        let mut backend = Recorder::default();
        dispatch(cli, &mut backend).unwrap();
        assert_eq!(backend.requests.len(), 1);
        assert_eq!(backend.requests[0].version, 1);
        let command = &backend.requests[0].command;
        if command["operation"] == "prompt" {
            assert_eq!(
                command["arguments"]["options"]["tool_grants"],
                json!(["read"])
            );
            assert_eq!(command["arguments"]["options"]["detach"], true);
        } else {
            assert_eq!(command["operation"], "run");
            assert_eq!(command["arguments"]["detach"], true);
        }
    }
    let mut cli = parse(&["task", "--tool-grant", "read"]);
    cli.command = Some(f5y::args::Command::Version);
    let mut backend = Recorder::default();
    assert_eq!(
        dispatch(cli, &mut backend).err().unwrap().message,
        "Root tool grants require a task prompt"
    );
    assert!(backend.requests.is_empty());
    let mut cli = parse(&["ls"]);
    cli.prompt = Some("task".into());
    assert_eq!(dispatch(cli, &mut backend).err().unwrap().code, "usage");
    assert!(backend.requests.is_empty());
}

#[test]
fn backend_nonzero_response_status_is_preserved_in_both_formats() {
    for status in [1, 3, 10, 75, 130, 255] {
        for selector in [vec![], vec!["--json"], vec!["--output-format", "json"]] {
            let mut args = vec!["status"];
            args.extend(&selector);
            let mut backend = Recorder {
                status,
                ..Recorder::default()
            };
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            assert_eq!(
                execute(parse(&args), &mut backend, &mut stdout, &mut stderr).unwrap(),
                status
            );
            assert!(stderr.is_empty());
            assert_eq!(backend.requests.len(), 1);
            if selector.is_empty() {
                assert_eq!(stdout, b"backend result\n");
            } else {
                assert_eq!(
                    serde_json::from_slice::<Value>(&stdout).unwrap(),
                    json!({"result":"backend result"})
                );
            }
        }
    }
}

#[test]
fn unsupported_errors_use_stderr_and_status_two() {
    for selector in [vec![], vec!["--json"], vec!["--output-format", "json"]] {
        let mut args = vec!["status"];
        args.extend(&selector);
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        assert_eq!(
            execute(
                parse(&args),
                &mut UnavailableBackend,
                &mut stdout,
                &mut stderr
            )
            .unwrap(),
            2
        );
        assert!(stdout.is_empty());
        if selector.is_empty() {
            assert_eq!(
                stderr,
                b"error: Foundry backend does not implement 'status' yet\n"
            );
        } else {
            let value: Value = serde_json::from_slice(&stderr).unwrap();
            assert_eq!(value["error"]["code"], "unsupported");
            assert_eq!(
                value["error"]["message"],
                "Foundry backend does not implement 'status' yet"
            );
        }
    }
}
