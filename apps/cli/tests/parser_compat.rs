use clap::Parser;
use f5y::{Backend, CommandError, Request, Response, args::Cli, dispatch};
use serde_json::{Value, json};

#[derive(Default)]
struct Recorder(Vec<Value>);

impl Backend for Recorder {
    fn execute(&mut self, request: Request) -> Result<Response, CommandError> {
        self.0.push(request.command);
        Ok(Response {
            text: String::new(),
            json: json!({}),
            exit_code: 0,
        })
    }
}

fn request(arguments: &[&str]) -> Value {
    let mut backend = Recorder::default();
    dispatch(Cli::try_parse_from(arguments).unwrap(), &mut backend).unwrap();
    backend.0.pop().unwrap()
}

#[test]
fn device_alias_preserves_connect_options() {
    assert_eq!(
        request(&["f5y", "device", "--name", "worker", "--yes"]),
        request(&["f5y", "connect", "--name", "worker", "--yes"])
    );
}

#[test]
fn standalone_upgrade_is_distinct_from_task_update() {
    let expected = request(&["f5y", "update"]);
    assert_eq!(request(&["f5y", "--upgrade"]), expected);
    assert_eq!(request(&["f5y", "upgrade"]), expected);
    let task = request(&["f5y", "task", "--update"]);
    assert_eq!(task["operation"], "prompt");
    assert_eq!(task["arguments"]["options"]["update"], true);
    assert_eq!(task["arguments"]["options"]["upgrade"], false);
}

#[test]
fn upgrade_rejects_task_options_before_backend() {
    for arguments in [
        vec!["f5y", "--upgrade", "task"],
        vec!["f5y", "--upgrade", "--model", "test"],
        vec!["f5y", "--upgrade", "status"],
        vec!["f5y", "--upgrade", "--update"],
    ] {
        let mut backend = Recorder::default();
        if let Ok(cli) = Cli::try_parse_from(&arguments) {
            assert!(dispatch(cli, &mut backend).is_err(), "{arguments:?}");
        }
        assert!(backend.0.is_empty());
    }
}

#[test]
fn message_validation_rejects_blank_and_preserves_nonblank_text() {
    for message in ["", " ", "\n\t"] {
        for arguments in [
            vec!["f5y", "send", "id", message],
            vec!["f5y", "session", "send", "id", "--message", message],
            vec!["f5y", "network", "team/agent", message],
        ] {
            assert!(Cli::try_parse_from(&arguments).is_err(), "{arguments:?}");
        }
    }
    let sent = request(&["f5y", "send", "id", "  continue\n"]);
    assert_eq!(sent["arguments"]["message"], "  continue\n");
}

fn help(arguments: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_f5y"))
        .args(arguments)
        .env_remove("FOUNDRY_DATABASE")
        .env("FOUNDRY_ADAPTER", "invalid-relative-adapter")
        .output()
        .unwrap()
}

#[test]
fn full_help_is_configuration_free_and_includes_nested_commands() {
    let output = help(&["help", "all"]);
    assert!(output.status.success(), "{:?}", output);
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    for command in [
        "f5y session start",
        "f5y config set",
        "f5y providers connect",
    ] {
        assert!(text.contains(command), "missing {command}");
    }
}

#[test]
fn nested_help_and_alias_help_remain_configuration_free() {
    for arguments in [
        vec!["help"],
        vec!["help", "session", "start"],
        vec!["help", "device"],
        vec!["help", "upgrade"],
        vec!["session", "start", "--help"],
    ] {
        let output = help(&arguments);
        assert!(output.status.success(), "{arguments:?}: {output:?}");
        assert!(output.stderr.is_empty());
        assert!(String::from_utf8(output.stdout).unwrap().contains("Usage:"));
    }
}

#[test]
fn invalid_help_path_fails_without_configuration_or_backend() {
    let output = help(&["help", "session", "missing", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "usage");
}
