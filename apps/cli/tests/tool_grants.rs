use clap::Parser;
use f5y::{
    Backend, CommandError, Request, Response, args::Cli, dispatch, runner::ExecutionBackend,
};
use serde_json::{Value, json};
use std::process::Command;
use tempfile::TempDir;

#[derive(Default)]
struct Recorder(Option<Request>);
impl Backend for Recorder {
    fn execute(&mut self, request: Request) -> Result<Response, CommandError> {
        self.0 = Some(request);
        Ok(Response {
            text: String::new(),
            json: Value::Null,
            exit_code: 0,
        })
    }
}
fn request(args: &[&str]) -> Request {
    let cli = Cli::try_parse_from(std::iter::once("f5y").chain(args.iter().copied())).unwrap();
    let mut recorder = Recorder::default();
    dispatch(cli, &mut recorder).unwrap();
    recorder.0.unwrap()
}
fn task_forms() -> [Vec<&'static str>; 3] {
    [
        vec!["fixture"],
        vec!["run", "--task", "fixture"],
        vec!["session", "start", "--prompt", "fixture"],
    ]
}
const POINTERS: [&str; 3] = ["/arguments/options", "/arguments", "/arguments/arguments"];
const GRANTS: [&str; 4] = ["read", "list", "write.create", "write.overwrite"];

#[test]
fn task_grants_preserve_exact_independent_repeated_wire_values() {
    for (form, pointer) in task_forms().iter().zip(POINTERS) {
        for grant in GRANTS {
            let mut args = form.clone();
            args.extend(["--tool-grant", grant]);
            assert_eq!(
                request(&args).command.pointer(pointer).unwrap()["tool_grants"],
                json!([grant])
            );
        }
        let mut args = form.clone();
        for grant in GRANTS.into_iter().chain(["read"]) {
            args.extend(["--tool-grant", grant]);
        }
        assert_eq!(
            request(&args).command.pointer(pointer).unwrap()["tool_grants"],
            json!(["read", "list", "write.create", "write.overwrite", "read"])
        );
    }
}

#[test]
fn absent_grants_preserve_existing_wire_and_yes_never_grants() {
    for (form, pointer) in task_forms().iter().zip(POINTERS) {
        assert!(
            request(form)
                .command
                .pointer(pointer)
                .unwrap()
                .get("tool_grants")
                .is_none()
        );
    }
    for yes in ["--yes", "-y"] {
        let command = request(&["fixture", yes]).command;
        assert_eq!(command["arguments"]["options"]["yes"], true);
        assert!(command["arguments"]["options"].get("tool_grants").is_none());
        let command = request(&["fixture", yes, "--tool-grant", "read"]).command;
        assert_eq!(
            command["arguments"]["options"]["tool_grants"],
            json!(["read"])
        );
    }
}

#[test]
fn parser_rejects_unknown_combined_or_non_task_grants() {
    for form in task_forms() {
        for invalid in ["all", "*", "write", "write-create", "read,list", "READ", ""] {
            let mut args = vec!["f5y"];
            args.extend(form.iter().copied());
            args.extend(["--tool-grant", invalid]);
            assert!(Cli::try_parse_from(args).is_err(), "{form:?}: {invalid}");
        }
    }
    for args in [
        vec!["f5y", "--tool-grant", "read"],
        vec!["f5y", "ls", "--tool-grant", "read"],
        vec!["f5y", "acp", "--tool-grant", "read"],
        vec!["f5y", "mcp", "--tool-grant", "read"],
        vec!["f5y", "watch", "s", "--tool-grant", "read"],
        vec!["f5y", "session", "get", "s", "--tool-grant", "read"],
        vec!["f5y", "send", "s", "text", "--tool-grant", "read"],
    ] {
        assert!(Cli::try_parse_from(&args).is_err(), "{args:?}");
    }
    // A root option placed before a subcommand must not get silently dropped.
    for args in [
        vec!["f5y", "--tool-grant", "read", "ls"],
        vec!["f5y", "fixture", "--tool-grant", "read", "--upgrade"],
    ] {
        let mut recorder = Recorder::default();
        if let Ok(cli) = Cli::try_parse_from(&args) {
            assert_eq!(dispatch(cli, &mut recorder).err().unwrap().code, "usage");
        }
        assert!(recorder.0.is_none());
    }
}

struct NoFallback;
impl Backend for NoFallback {
    fn execute(&mut self, _: Request) -> Result<Response, CommandError> {
        panic!("grant request reached fallback");
    }
}

#[test]
fn explicit_grants_require_execution_before_queue_fallback_or_database() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("state.db");
    for configured in [false, true] {
        let adapter = configured.then(|| directory.path().join("missing-adapter"));
        let mut backend = ExecutionBackend::new(&database, NoFallback, adapter).unwrap();
        for mut form in task_forms() {
            form.extend(["--tool-grant", "write.create"]);
            let error = backend.execute(request(&form)).err().unwrap();
            assert_eq!(
                error.code,
                if configured {
                    "invalid"
                } else {
                    "adapter_not_configured"
                }
            );
        }
    }
    assert!(!database.exists());
}

#[test]
fn cli_grants_require_adapter_configuration_without_side_effects() {
    let directory = TempDir::new().unwrap();
    for configured in [false, true] {
        for form in task_forms() {
            for grant in GRANTS {
                let mut command = Command::new(env!("CARGO_BIN_EXE_f5y"));
                command
                    .current_dir(directory.path())
                    .env_remove("FOUNDRY_DATABASE")
                    .env_remove("FOUNDRY_ADAPTER")
                    .env_remove("FOUNDRY_TOOLHOST")
                    .args(["--database", "state.db"]);
                if configured {
                    command.env("FOUNDRY_ADAPTER", directory.path().join("missing-adapter"));
                }
                let output = command
                    .args(&form)
                    .args(["--tool-grant", grant, "--json"])
                    .output()
                    .unwrap();
                assert_eq!(output.status.code(), Some(2));
                assert!(output.stdout.is_empty());
                let error: Value = serde_json::from_slice(&output.stderr).unwrap();
                assert_eq!(
                    error["error"]["code"],
                    if configured {
                        "invalid"
                    } else {
                        "adapter_not_configured"
                    },
                    "{error}"
                );
            }
        }
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}
