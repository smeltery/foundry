use clap::Parser;
use f5y::{Backend, CommandError, Request, Response, args::Cli, dispatch};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, process::Command};

#[derive(Default)]
struct Recorder(Vec<Value>);
impl Backend for Recorder {
    fn execute(&mut self, request: Request) -> Result<Response, CommandError> {
        self.0.push(serde_json::to_value(request).unwrap());
        Ok(Response {
            text: String::new(),
            json: Value::Null,
            exit_code: 0,
        })
    }
}

fn routes() -> [Vec<&'static str>; 4] {
    [
        vec!["task"],
        vec!["run", "--task", "task"],
        vec!["session", "start", "--prompt", "task"],
        vec!["acp"],
    ]
}
fn parse(argv: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("f5y").chain(argv.iter().copied())).unwrap()
}
fn expected(route: usize, effort: Option<&str>, model: Option<&str>) -> Value {
    let options = json!({"cloud":false,"on":null,"dir":null,"yes":false,"detach":false,
        "repo":null,"no_repo":false,"model":model,"effort":effort,"resume":null,"update":false,"upgrade":false});
    let command = match route {
        0 => json!({"operation":"prompt","arguments":{"prompt":"task","options":options}}),
        1 => {
            json!({"operation":"run","arguments":{"task":"task","repo":null,"git_ref":null,"model":model,"effort":effort,"detach":false}})
        }
        2 => {
            json!({"operation":"session","arguments":{"operation":"start","arguments":{"prompt":"task","repo":null,"provider":null,"model":model,"effort":effort}}})
        }
        3 => json!({"operation":"acp","arguments":options}),
        _ => unreachable!(),
    };
    json!({"version":1,"command":command})
}

#[test]
fn all_routes_preserve_literal_effort_and_omitted_optional_values() {
    for (index, argv) in routes().into_iter().enumerate() {
        for effort in [
            None,
            Some("low"),
            Some("medium"),
            Some("high"),
            Some("xhigh"),
        ] {
            let mut argv = argv.clone();
            if let Some(value) = effort {
                argv.extend(["--effort", value]);
            }
            let mut recorder = Recorder::default();
            dispatch(parse(&argv), &mut recorder).unwrap();
            assert_eq!(recorder.0, vec![expected(index, effort, None)], "{argv:?}");
        }
    }
}

#[test]
fn missing_and_invalid_effort_reject_before_dispatch_or_configuration() {
    let directory = tempfile::tempdir().unwrap();
    for argv in routes() {
        for value in [None, Some(""), Some("invalid"), Some("HIGH")] {
            let mut argv = argv.clone();
            argv.push("--effort");
            if let Some(value) = value {
                argv.push(value);
            }
            let parsed = Cli::try_parse_from(std::iter::once("f5y").chain(argv.iter().copied()));
            assert!(parsed.is_err(), "{argv:?}");
            let output = binary(directory.path()).args(&argv).output().unwrap();
            assert_eq!(output.status.code(), Some(2), "{argv:?}");
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(stderr.contains("--effort"), "{stderr}");
            assert!(!stderr.contains("Select a database"), "{stderr}");
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        }
    }
}

fn binary(directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_f5y"));
    command
        .current_dir(directory)
        .env_remove("FOUNDRY_DATABASE")
        .env_remove("FOUNDRY_ADAPTER");
    command
}

#[test]
fn each_route_help_lists_the_original_effort_option_and_all_values() {
    let directory = tempfile::tempdir().unwrap();
    for route in [vec![], vec!["run"], vec!["session", "start"], vec!["acp"]] {
        let output = binary(directory.path())
            .args(&route)
            .arg("--help")
            .output()
            .unwrap();
        assert!(output.status.success(), "{route:?}");
        assert!(output.stderr.is_empty());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("--effort"), "{route:?}");
        for value in ["low", "medium", "high", "xhigh"] {
            assert!(
                help.split(|c: char| !c.is_ascii_alphabetic())
                    .any(|word| word == value),
                "{route:?}: {value}"
            );
        }
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}

#[test]
fn local_execution_rejects_every_effort_before_adapter_or_database_access() {
    use f5y::{local::LocalBackend, runner::ExecutionBackend};
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    for argv in routes().into_iter().take(3) {
        for value in ["low", "medium", "high", "xhigh"] {
            for configured in [false, true] {
                let mut argv = argv.clone();
                argv.extend(["--effort", value]);
                // A nonexistent absolute adapter must not be resolved or executed.
                let adapter = configured.then(|| directory.path().join("missing-adapter"));
                let mut backend =
                    ExecutionBackend::new(&database, LocalBackend::new(&database), adapter)
                        .unwrap();
                let error = dispatch(parse(&argv), &mut backend)
                    .err()
                    .expect("unsupported effort");
                assert_eq!(error.code, "unsupported", "{argv:?}: {error:?}");
                assert!(error.message.contains("effort"), "{error:?}");
                assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
            }
        }
    }
}

fn files(directory: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn actual_startup_rejects_effort_without_creating_or_changing_sessions() {
    for seeded in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("state.db");
        if seeded {
            let output = binary(directory.path())
                .env("FOUNDRY_DATABASE", &database)
                .args(["session", "start", "--prompt", "existing session", "--json"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let result: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["execution"], "not_started");
            assert_eq!(result["session"]["prompt"], "existing session");
        }
        let before = files(directory.path());
        for argv in routes().into_iter().take(3) {
            for configured in [false, true] {
                let mut command = binary(directory.path());
                command
                    .env("FOUNDRY_DATABASE", &database)
                    .args(&argv)
                    .args(["--effort", "high", "--json"]);
                if configured {
                    command.env("FOUNDRY_ADAPTER", directory.path().join("missing-adapter"));
                }
                let output = command.output().unwrap();
                assert_eq!(output.status.code(), Some(2), "{argv:?}");
                assert!(output.stdout.is_empty());
                let error: Value = serde_json::from_slice(&output.stderr).unwrap();
                assert_eq!(error["error"]["code"], "unsupported", "{argv:?}: {error}");
                assert!(
                    error["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("effort")
                );
                assert_eq!(files(directory.path()), before, "{argv:?}");
            }
        }
    }
}

#[test]
fn model_bytes_and_effort_are_independent_on_every_route() {
    for (index, route) in routes().into_iter().enumerate() {
        for effort in [None, Some("high")] {
            let mut argv = route.clone();
            argv.extend(["--model", "  fixture-model/λ  "]);
            if let Some(value) = effort {
                argv.extend(["--effort", value]);
            }
            let mut recorder = Recorder::default();
            dispatch(parse(&argv), &mut recorder).unwrap();
            assert_eq!(
                recorder.0,
                vec![expected(index, effort, Some("  fixture-model/λ  "))],
                "{argv:?}"
            );
        }
    }
}

#[test]
fn root_effort_cannot_be_discarded_by_unrelated_subcommands() {
    for command in [vec!["status"], vec!["ls"], vec!["config", "list"]] {
        let argv: Vec<_> = ["--effort", "high"].into_iter().chain(command).collect();
        let mut recorder = Recorder::default();
        let error = dispatch(parse(&argv), &mut recorder)
            .err()
            .expect("root option conflict");
        assert_eq!(error.code, "usage", "{argv:?}");
        assert!(recorder.0.is_empty());
    }
}

#[test]
fn direct_store_backend_guards_effort_and_retains_legacy_absence() {
    use f5y::local::LocalBackend;
    for seeded in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("state.db");
        if seeded {
            let mut backend = LocalBackend::new(&database);
            for args in [
                json!({"prompt":"legacy"}),
                json!({"prompt":"null effort","effort":null}),
            ] {
                let result = backend.execute(Request {
                    version: 1,
                    command: json!({"operation":"session","arguments":{"operation":"start","arguments":args}}),
                }).unwrap();
                assert_eq!(result.json["execution"], "not_started");
                assert_eq!(result.json["session"]["prompt"], args["prompt"]);
            }
        }
        let before = files(directory.path());
        for value in ["low", "medium", "high", "xhigh"] {
            let mut backend = LocalBackend::new(&database);
            let error = backend.execute(Request {
                version: 1,
                command: json!({"operation":"session","arguments":{"operation":"start","arguments":{"prompt":"must not queue","effort":value}}}),
            }).err().expect("direct guard");
            assert_eq!(error.code, "unsupported");
            assert!(error.message.contains("effort"));
            drop(backend);
            assert_eq!(files(directory.path()), before);
        }
    }
}
