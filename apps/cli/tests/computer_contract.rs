use clap::Parser;
use f5y::{Backend, CommandError, Request, Response, args::Cli, dispatch};
use serde_json::json;

#[derive(Default)]
struct Recorder(Option<Request>);

impl Backend for Recorder {
    fn execute(&mut self, request: Request) -> Result<Response, CommandError> {
        self.0 = Some(request);
        Ok(Response {
            text: String::new(),
            json: json!({}),
            exit_code: 0,
        })
    }
}

#[test]
fn worker_requests_preserve_target_and_execution_options() {
    let mut backend = Recorder::default();
    let cli = Cli::try_parse_from([
        "f5y",
        "serve",
        "build",
        "--dir",
        "/tmp/work",
        "--service",
        "--yes",
    ])
    .unwrap();
    dispatch(cli, &mut backend).unwrap();
    let request = backend.0.unwrap();
    assert_eq!(request.command["operation"], "serve");
    assert_eq!(request.command["arguments"]["name"], "build");
    assert_eq!(request.command["arguments"]["dir"], "/tmp/work");
    assert_eq!(request.command["arguments"]["service"], true);
}

#[test]
fn connect_preserves_explicit_privacy_options() {
    let cli = Cli::try_parse_from([
        "f5y",
        "connect",
        "--local-logins",
        "off",
        "--user-chrome",
        "off",
        "--folder",
        "/tmp/work",
        "--add-folder",
        "/tmp/extra",
    ])
    .unwrap();
    let mut backend = Recorder::default();
    dispatch(cli, &mut backend).unwrap();
    let arguments = &backend.0.as_ref().unwrap().command["arguments"];
    assert_eq!(arguments["local_logins"], "off");
    assert_eq!(arguments["user_chrome"], "off");
    assert_eq!(arguments["add_folder"], json!(["/tmp/extra"]));
}

#[test]
fn account_desktop_and_queue_commands_parse() {
    for arguments in [
        vec!["f5y", "machines", "token", "build"],
        vec!["f5y", "machines", "rm", "build", "--yes"],
        vec!["f5y", "machines", "--json"],
        vec!["f5y", "disconnect", "build", "--yes"],
        vec!["f5y", "desktop", "install", "--permissions", "standard"],
        vec!["f5y", "desktop", "browser-use", "--json"],
        vec!["f5y", "permissions", "allow", "disk"],
        vec!["f5y", "login"],
        vec!["f5y", "logout"],
        vec!["f5y", "whoami"],
        vec!["f5y", "upgrade", "--check", "--quiet"],
        vec!["f5y", "providers", "connect", "anthropic", "--token-stdin"],
        vec!["f5y", "acp", "--on", "build", "--model", "test"],
    ] {
        Cli::try_parse_from(&arguments).unwrap_or_else(|error| panic!("{arguments:?}: {error}"));
    }
}

#[test]
fn credential_modes_and_worker_lifetime_are_exclusive() {
    for arguments in [
        vec![
            "f5y",
            "providers",
            "connect",
            "test",
            "--browser",
            "--token-stdin",
        ],
        vec!["f5y", "serve", "--once", "--service"],
        vec!["f5y", "serve", "build", "--token", "fixture"],
        vec!["f5y", "permissions", "allow", "unknown"],
    ] {
        assert!(Cli::try_parse_from(&arguments).is_err(), "{arguments:?}");
    }
}
