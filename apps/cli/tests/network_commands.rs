#[path = "support_network/cases.rs"]
mod cases;

use clap::Parser;
use f5y::{
    Backend, CommandError, Request, Response, args::Cli, dispatch, network_args::NetworkCli,
    network_cli,
};
use serde_json::{Value, json};

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

fn composed(argv: &[&str]) -> Cli {
    Cli::try_parse_from(["f5y", "network"].into_iter().chain(argv.iter().copied()))
        .unwrap_or_else(|error| panic!("{argv:?}: {error}"))
}

fn standalone(argv: &[&str]) -> NetworkCli {
    NetworkCli::try_parse_from(std::iter::once("f5y-network").chain(argv.iter().copied()))
        .unwrap_or_else(|error| panic!("{argv:?}: {error}"))
}

fn expected(command: Value) -> Value {
    json!({"version":1,"command":{"operation":"network","arguments":{
        "command":command,"address":null,"message":null
    }}})
}

#[test]
fn complete_command_catalog_preserves_defaults_and_supplied_values() {
    for (argv, command) in cases::defaults().into_iter().chain(cases::explicit()) {
        let mut recorder = Recorder::default();
        dispatch(composed(&argv), &mut recorder)
            .unwrap_or_else(|error| panic!("{argv:?}: {error:?}"));
        assert_eq!(recorder.0, vec![expected(command)], "{argv:?}");
        let mut other = Recorder::default();
        network_cli::dispatch(standalone(&argv), &mut other)
            .unwrap_or_else(|error| panic!("{argv:?}: {error:?}"));
        assert_eq!(other.0, recorder.0, "{argv:?}");
    }
}

#[test]
fn missing_values_invalid_enums_numeric_bounds_and_conflicts_reject_parsing() {
    for argv in [
        vec!["send"],
        vec!["proof"],
        vec!["remove"],
        vec!["relay"],
        vec!["folder", "/workspace"],
        vec!["folder", "/workspace", "maybe"],
        vec!["folder", "/workspace", "OFF"],
        vec!["hook"],
        vec!["hook", "fixture-agent"],
        vec!["send", "person", "--fyi", "--notify"],
        vec!["send", "person", "--file"],
        vec!["send", "person", "--reply-to"],
        vec!["send", "person", "--from"],
        vec!["status", "--name"],
        vec!["init", "--as"],
        vec!["init", "--relay"],
        vec!["init", "--proof"],
        vec!["join", "--as"],
        vec!["join", "--proof"],
        vec!["listen", "--as"],
        vec!["listen", "--wait"],
        vec!["invite", "--hours"],
        vec!["hook", "fixture-agent", "fixture-event", "--session"],
        vec!["hook", "fixture-agent", "fixture-event", "--state"],
        vec!["hook", "fixture-agent", "fixture-event", "--doing"],
        vec!["listen", "--wait=-1"],
        vec!["invite", "--hours=-1"],
        vec!["listen", "--wait=18446744073709551616"],
        vec!["invite", "--hours=18446744073709551616"],
        vec!["listen", "--wait=1.5"],
        vec!["invite", "--hours=many"],
    ] {
        assert!(
            NetworkCli::try_parse_from(std::iter::once("f5y-network").chain(argv.iter().copied()))
                .is_err(),
            "{argv:?}"
        );
        assert!(
            Cli::try_parse_from(["f5y", "network"].into_iter().chain(argv.iter().copied()))
                .is_err(),
            "{argv:?}"
        );
    }
}

#[test]
fn empty_held_selection_is_a_semantic_error_before_backend_dispatch() {
    for name in ["approve", "reject"] {
        let cli = composed(&[name]);
        let mut recorder = Recorder::default();
        let error = dispatch(cli, &mut recorder).err().expect("empty selection");
        assert_eq!(error.code, "usage", "{name}");
        assert!(recorder.0.is_empty(), "{name}");
        let error = network_cli::dispatch(standalone(&[name]), &mut recorder)
            .err()
            .expect("empty selection");
        assert_eq!(error.code, "usage", "{name}");
        assert!(recorder.0.is_empty(), "{name}");
    }
}

#[test]
fn composed_json_aliases_only_forward_to_commands_with_json_support() {
    for name in ["who", "init", "join", "link", "team"] {
        let (_, mut command) = cases::defaults()
            .into_iter()
            .find(|(argv, _)| argv.first() == Some(&name))
            .unwrap();
        command["arguments"]["json"] = json!(true);
        for argv in [
            vec!["f5y", "network", name, "--json"],
            vec!["f5y", "--json", "network", name],
            vec!["f5y", "network", name, "--output-format=json"],
            vec!["f5y", "--output-format", "json", "network", name],
        ] {
            let mut recorder = Recorder::default();
            let cli = Cli::try_parse_from(&argv).unwrap();
            dispatch(cli, &mut recorder).unwrap_or_else(|error| panic!("{argv:?}: {error:?}"));
            assert_eq!(recorder.0, vec![expected(command.clone())], "{argv:?}");
        }
    }
    for (argv, _) in cases::defaults() {
        if argv
            .first()
            .is_some_and(|name| ["who", "init", "join", "link", "team"].contains(name))
        {
            continue;
        }
        for format in ["--json", "--output-format=json"] {
            let args: Vec<_> = ["f5y", format, "network"]
                .into_iter()
                .chain(argv.iter().copied())
                .collect();
            let cli = Cli::try_parse_from(&args).unwrap();
            let mut recorder = Recorder::default();
            assert!(dispatch(cli, &mut recorder).is_err(), "{args:?}");
            assert!(recorder.0.is_empty(), "{args:?}");
        }
    }
}

#[test]
fn network_names_do_not_dispatch_root_operations() {
    for (network, root, expected_root) in [
        (
            vec!["status"],
            vec!["status"],
            json!({"operation":"status"}),
        ),
        (
            vec!["stop"],
            vec!["stop", "worker"],
            json!({"operation":"stop","arguments":{"name":"worker"}}),
        ),
        (
            vec!["serve", "adapter"],
            vec!["serve", "worker"],
            json!({"operation":"serve","arguments":{"name":"worker","token":null,"dir":null,"once":false,"service":false,"sandbox":false,"yes":false,"stop":false}}),
        ),
        (
            vec!["upgrade"],
            vec!["upgrade"],
            json!({"operation":"update","arguments":{"check":false,"quiet":false}}),
        ),
        (vec!["mcp"], vec!["mcp"], json!({"operation":"mcp"})),
    ] {
        let mut network_recorder = Recorder::default();
        dispatch(composed(&network), &mut network_recorder).unwrap();
        assert_eq!(network_recorder.0.len(), 1);
        assert_eq!(network_recorder.0[0]["command"]["operation"], "network");
        let mut root_recorder = Recorder::default();
        let cli = Cli::try_parse_from(std::iter::once("f5y").chain(root)).unwrap();
        dispatch(cli, &mut root_recorder).unwrap();
        assert_eq!(
            root_recorder.0,
            vec![json!({"version":1,"command":expected_root})]
        );
        assert_ne!(network_recorder.0, root_recorder.0);
    }
}

#[test]
fn hidden_operational_routes_parse_but_are_absent_from_network_help() {
    for error in [
        Cli::try_parse_from(["f5y", "network", "--help"]).unwrap_err(),
        NetworkCli::try_parse_from(["f5y-network", "--help"]).unwrap_err(),
    ] {
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        let help = error.to_string();
        for name in ["who", "send", "listen", "folder", "approve", "reject"] {
            assert!(help.contains(name), "{name}");
        }
        for line in help.lines() {
            let first = line.split_whitespace().next();
            assert!(!matches!(first, Some("hook" | "tray")));
        }
    }
    for argv in [vec!["hook", "fixture-agent", "fixture-event"], vec!["tray"]] {
        let mut recorder = Recorder::default();
        dispatch(composed(&argv), &mut recorder).unwrap();
        assert_eq!(recorder.0.len(), 1);
    }
}

#[test]
fn configured_unsupported_network_binary_does_not_create_state() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    for argv in [
        vec!["network", "who"],
        vec!["network", "who", "--json"],
        vec![
            "network",
            "send",
            "person/session",
            "message",
            "--file",
            "missing.txt",
        ],
        vec!["network", "setup", "--dry-run"],
    ] {
        for (binary, args) in [
            (env!("CARGO_BIN_EXE_f5y"), argv.as_slice()),
            (env!("CARGO_BIN_EXE_f5y-network"), &argv[1..]),
        ] {
            let output = std::process::Command::new(binary)
                .current_dir(directory.path())
                .env("FOUNDRY_DATABASE", &database)
                .env_remove("FOUNDRY_ADAPTER")
                .args(args)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2), "{argv:?}");
            assert!(output.stdout.is_empty(), "{argv:?}");
            let stderr = String::from_utf8(output.stderr).unwrap();
            if argv.contains(&"--json") {
                let error: Value = serde_json::from_str(&stderr).unwrap();
                assert_eq!(error["error"]["code"], "unsupported");
            } else {
                let message = if binary == env!("CARGO_BIN_EXE_f5y") {
                    "error: Local store backend does not support network\n"
                } else {
                    "error: Foundry backend does not implement 'network' yet\n"
                };
                assert_eq!(stderr, message);
            }
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        }
    }
}

#[test]
fn standalone_does_not_inherit_composed_format_or_wrapper_options() {
    for argv in [
        vec!["--json", "who"],
        vec!["who", "--output-format", "json"],
        vec!["--database", "state.db"],
        vec!["serve", "adapter"],
        vec!["person/session", "question"],
    ] {
        assert!(NetworkCli::try_parse_from(std::iter::once("f5y-network").chain(argv)).is_err());
    }
    for (mut argv, _) in cases::defaults() {
        if argv
            .first()
            .is_some_and(|name| ["who", "init", "join", "link", "team"].contains(name))
        {
            continue;
        }
        argv.push("--json");
        assert!(
            NetworkCli::try_parse_from(std::iter::once("f5y-network").chain(argv.iter().copied()))
                .is_err(),
            "{argv:?}"
        );
    }
}

#[test]
fn explicit_blank_message_rejects_after_parse_without_consuming_stdin() {
    for argv in [
        vec!["send", "person/session", "  "],
        vec!["send", "person/session", "", "\t"],
    ] {
        let mut recorder = Recorder::default();
        assert_eq!(
            dispatch(composed(&argv), &mut recorder).err().unwrap().code,
            "usage"
        );
        assert_eq!(
            network_cli::dispatch(standalone(&argv), &mut recorder)
                .err()
                .unwrap()
                .code,
            "usage"
        );
        assert!(recorder.0.is_empty());
    }
}
