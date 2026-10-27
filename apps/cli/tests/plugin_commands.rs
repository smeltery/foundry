use clap::Parser;
use f5y::{Backend, CommandError, Request, Response, args::Cli, dispatch, execute};
use serde_json::{Value, json};
use tempfile::TempDir;

#[derive(Default)]
struct Recorder(Vec<Value>);
impl Backend for Recorder {
    fn execute(&mut self, request: Request) -> Result<Response, CommandError> {
        self.0.push(serde_json::to_value(request).unwrap());
        Ok(Response {
            text: "fixture result".into(),
            json: json!({"fixture":"result"}),
            exit_code: 0,
        })
    }
}

fn parse(argv: &[&str]) -> Cli {
    Cli::try_parse_from(["f5y", "plugins"].into_iter().chain(argv.iter().copied()))
        .unwrap_or_else(|error| panic!("{argv:?}: {error}"))
}

fn request(operation: &str, arguments: Value) -> Value {
    json!({"version":1,"command":{"operation":"plugins","arguments":{
        "operation":operation,"arguments":arguments
    }}})
}

fn check(argv: &[&str], expected: Value) {
    let mut recorder = Recorder::default();
    dispatch(parse(argv), &mut recorder).unwrap_or_else(|error| panic!("{argv:?}: {error:?}"));
    assert_eq!(recorder.0, vec![expected], "{argv:?}");
}

#[test]
fn required_clients_scope_enums_and_management_namespace_reject_invalid_grammar() {
    for argv in [
        vec!["add", "package"],
        vec!["remove", "package"],
        vec!["add", "--client", "codex"],
        vec!["remove", "--client", "codex"],
        vec!["list", "--client"],
        vec!["add", "package", "--client"],
        vec!["remove", "package", "--client"],
        vec!["list", "--scope", "other"],
        vec!["add", "package", "--client", "codex", "--scope", "all"],
        vec!["remove", "package", "--client", "codex", "--scope", "all"],
        vec!["list", "--project"],
        vec!["add", "package", "--client", "codex", "--version"],
        vec!["remove", "package", "--client", "codex", "--replace"],
        vec!["add", "package", "--client", "codex", "--force"],
    ] {
        assert!(
            Cli::try_parse_from(["f5y", "plugins"].into_iter().chain(argv.iter().copied()))
                .is_err(),
            "{argv:?}"
        );
    }
    for command in [
        vec!["list"],
        vec!["add", "package"],
        vec!["remove", "package"],
    ] {
        let mut argv = command;
        argv.extend(["--client", "unsupported-client"]);
        assert!(Cli::try_parse_from(["f5y", "plugins"].into_iter().chain(argv)).is_err());
    }
    for workflow in ["ask", "build", "fix", "fix-ci", "review", "status"] {
        assert!(
            Cli::try_parse_from(["f5y", "plugins", workflow]).is_err(),
            "{workflow}"
        );
    }
}

#[test]
fn project_with_user_scope_is_semantically_rejected_before_backend() {
    for command in [
        vec!["list"],
        vec!["add", "package", "--client", "codex"],
        vec!["remove", "package", "--client", "codex"],
    ] {
        for explicit in [false, true] {
            let mut argv = command.clone();
            argv.extend(["--project", "/missing/project"]);
            if explicit {
                argv.extend(["--scope", "user"]);
            }
            for format in ["--output-format=text", "--json", "--output-format=json"] {
                let mut argv = argv.clone();
                argv.push(format);
                let mut recorder = Recorder::default();
                let error = dispatch(parse(&argv), &mut recorder)
                    .err()
                    .expect("invalid scope");
                assert_eq!(error.code, "usage", "{argv:?}");
                let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
                assert_eq!(
                    execute(parse(&argv), &mut recorder, &mut stdout, &mut stderr).unwrap(),
                    2
                );
                assert!(stdout.is_empty());
                if format == "--output-format=text" {
                    assert!(String::from_utf8(stderr).unwrap().starts_with("error:"));
                } else {
                    let error: Value = serde_json::from_slice(&stderr).unwrap();
                    assert_eq!(error["error"]["code"], "usage");
                }
                assert!(recorder.0.is_empty(), "{argv:?}");
            }
        }
    }
}

#[test]
fn startup_rejects_invalid_scope_before_configuration_and_does_not_create_state() {
    let directory = TempDir::new().unwrap();
    for argv in [
        vec!["list"],
        vec!["add", "package", "--client", "codex"],
        vec!["remove", "package", "--client", "codex"],
    ] {
        for configured in [false, true] {
            let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_f5y"));
            command
                .current_dir(directory.path())
                .env_remove("FOUNDRY_DATABASE")
                .env("FOUNDRY_ADAPTER", "invalid-relative-path")
                .args(["plugins"])
                .args(&argv)
                .args(["--project", "missing", "--json"]);
            if configured {
                command.env("FOUNDRY_DATABASE", directory.path().join("state.db"));
            }
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(2));
            assert!(output.stdout.is_empty());
            let error: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(error["error"]["code"], "usage", "{argv:?}");
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        }
    }
}

#[test]
fn complete_requests_preserve_defaults_all_clients_scopes_and_flags() {
    check(
        &["list"],
        request(
            "list",
            json!({"available":false,"client":null,"scope":"user","project":null}),
        ),
    );
    for client in ["claude", "codex", "cursor", "grok"] {
        check(
            &["list", "--client", client],
            request(
                "list",
                json!({"available":false,"client":client,"scope":"user","project":null}),
            ),
        );
        check(
            &["add", "package", "--client", client],
            request(
                "add",
                json!({"name":"package","version":null,"client":client,"scope":"user","project":null,"replace":false,"dry_run":false,"yes":false}),
            ),
        );
        check(
            &["remove", "package", "--client", client],
            request(
                "remove",
                json!({"name":"package","client":client,"scope":"user","project":null,"dry_run":false,"yes":false}),
            ),
        );
        check(
            &[
                "add",
                "./local package",
                "--version",
                "1.2.3",
                "--client",
                client,
                "--scope",
                "project",
                "--project",
                "./missing project",
                "--replace",
                "--dry-run",
                "--yes",
            ],
            request(
                "add",
                json!({"name":"./local package","version":"1.2.3","client":client,"scope":"project","project":"./missing project","replace":true,"dry_run":true,"yes":true}),
            ),
        );
        check(
            &[
                "remove",
                "Package.ID",
                "--client",
                client,
                "--scope",
                "project",
                "--project",
                "./missing project",
                "--dry-run",
                "--yes",
            ],
            request(
                "remove",
                json!({"name":"Package.ID","client":client,"scope":"project","project":"./missing project","dry_run":true,"yes":true}),
            ),
        );
        for scope in ["project", "all"] {
            check(
                &[
                    "list",
                    "--available",
                    "--client",
                    client,
                    "--scope",
                    scope,
                    "--project",
                    "./missing project",
                ],
                request(
                    "list",
                    json!({"available":true,"client":client,"scope":scope,"project":"./missing project"}),
                ),
            );
        }
    }
    // Absent project remains distinct; discovery must be done by the backend.
    for scope in ["user", "project", "all"] {
        check(
            &["list", "--scope", scope],
            request(
                "list",
                json!({"available":false,"client":null,"scope":scope,"project":null}),
            ),
        );
    }
    for scope in ["user", "project"] {
        check(
            &["add", "package", "--client", "codex", "--scope", scope],
            request(
                "add",
                json!({"name":"package","version":null,"client":"codex","scope":scope,"project":null,"replace":false,"dry_run":false,"yes":false}),
            ),
        );
        check(
            &["remove", "package", "--client", "codex", "--scope", scope],
            request(
                "remove",
                json!({"name":"package","client":"codex","scope":scope,"project":null,"dry_run":false,"yes":false}),
            ),
        );
    }
}

#[test]
fn exported_execute_uses_shared_output_aliases_without_changing_requests() {
    for (argv, expected) in [
        (
            vec!["list"],
            request(
                "list",
                json!({"available":false,"client":null,"scope":"user","project":null}),
            ),
        ),
        (
            vec!["add", "package", "--client", "codex"],
            request(
                "add",
                json!({"name":"package","version":null,"client":"codex","scope":"user","project":null,"replace":false,"dry_run":false,"yes":false}),
            ),
        ),
        (
            vec!["remove", "package", "--client", "codex"],
            request(
                "remove",
                json!({"name":"package","client":"codex","scope":"user","project":null,"dry_run":false,"yes":false}),
            ),
        ),
    ] {
        for format in [
            vec![],
            vec!["--output-format", "text"],
            vec!["--json"],
            vec!["--output-format", "json"],
        ] {
            for prefix in [true, false] {
                let mut args = vec!["f5y"];
                if prefix {
                    args.extend(format.clone());
                }
                args.push("plugins");
                args.extend(argv.clone());
                if !prefix {
                    args.extend(format.clone());
                }
                let cli = Cli::try_parse_from(&args).unwrap();
                let mut recorder = Recorder::default();
                let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
                assert_eq!(
                    execute(cli, &mut recorder, &mut stdout, &mut stderr).unwrap(),
                    0
                );
                assert_eq!(recorder.0, vec![expected.clone()], "{args:?}");
                assert!(stderr.is_empty());
                if format.contains(&"--json") || format.contains(&"json") {
                    assert_eq!(stdout, b"{\"fixture\":\"result\"}\n");
                } else {
                    assert_eq!(stdout, b"fixture result\n");
                }
            }
        }
    }
}

#[test]
fn configured_startup_reports_unavailable_without_applying_package_operations() {
    let directory = TempDir::new().unwrap();
    for argv in [
        vec!["list"],
        vec![
            "list",
            "--available",
            "--scope",
            "all",
            "--project",
            "missing",
        ],
        vec!["add", "./missing", "--client", "codex"],
        vec![
            "add",
            "./missing",
            "--client",
            "codex",
            "--replace",
            "--dry-run",
            "--yes",
        ],
        vec!["remove", "package", "--client", "codex"],
        vec![
            "remove",
            "package",
            "--client",
            "codex",
            "--dry-run",
            "--yes",
        ],
    ] {
        for format in ["--output-format=text", "--json", "--output-format=json"] {
            let output = std::process::Command::new(env!("CARGO_BIN_EXE_f5y"))
                .current_dir(directory.path())
                .env("FOUNDRY_DATABASE", directory.path().join("state.db"))
                .env_remove("FOUNDRY_ADAPTER")
                .args(["plugins"])
                .args(&argv)
                .arg(format)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2));
            assert!(output.stdout.is_empty());
            if format == "--output-format=text" {
                assert_eq!(
                    output.stderr,
                    b"error: Local store backend does not support plugins\n"
                );
            } else {
                let error: Value = serde_json::from_slice(&output.stderr).unwrap();
                assert_eq!(error["error"]["code"], "unsupported");
            }
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        }
    }
}
