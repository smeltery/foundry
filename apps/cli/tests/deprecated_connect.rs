use clap::{CommandFactory, Parser};
use f5y::{
    Backend, CommandError, Request, Response,
    args::{Cli, Command},
    dispatch, execute,
};
use serde_json::{Value, json};

#[derive(Default)]
struct Recorder(Vec<Value>);
impl Backend for Recorder {
    fn execute(&mut self, request: Request) -> Result<Response, CommandError> {
        self.0.push(serde_json::to_value(request).unwrap());
        Ok(Response {
            text: "connected".into(),
            json: json!({"connected":true}),
            exit_code: 0,
        })
    }
}
fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once("f5y").chain(args.iter().copied()))
}
fn request(args: &[&str]) -> Value {
    let mut recorder = Recorder::default();
    dispatch(parse(args).unwrap(), &mut recorder).unwrap();
    assert_eq!(recorder.0.len(), 1);
    recorder.0.pop().unwrap()
}

#[test]
fn documented_positive_values_are_inert_for_connect_and_device() {
    let baseline = request(&["connect"]);
    for command in ["connect", "device"] {
        let cli = parse(&[command]).unwrap();
        let Some(Command::Connect(options)) = cli.command else {
            panic!("connect route")
        };
        assert!(options.sessions.is_none());
        assert!(options.cloud_sessions.is_none());
        assert_eq!(request(&[command]), baseline);
        for value in ["1", "4"] {
            assert_eq!(request(&[command, "--sessions", value]), baseline);
            for toggle in ["on", "off"] {
                assert_eq!(
                    request(&[command, "--sessions", value, "--cloud-sessions", toggle]),
                    baseline
                );
            }
        }
        for toggle in ["on", "off"] {
            assert_eq!(request(&[command, "--cloud-sessions", toggle]), baseline);
        }
    }
    assert!(baseline["command"]["arguments"].get("sessions").is_none());
    assert!(
        baseline["command"]["arguments"]
            .get("cloud_sessions")
            .is_none()
    );
}

#[test]
fn deprecated_options_preserve_all_explicit_connection_settings_and_clean_output() {
    let settings = [
        "--name",
        "worker",
        "--folder",
        "/tmp/work",
        "--add-folder",
        "/tmp/add",
        "--remove-folder",
        "/tmp/remove",
        "--api-key",
        "synthetic-key",
        "--api-url",
        "https://example.invalid",
        "--proxy",
        "http://proxy.invalid",
        "--proxy-ca-file",
        "/tmp/ca",
        "--user-chrome",
        "off",
        "--local-logins",
        "on",
        "--keep-awake",
        "off",
        "--once",
        "--yes",
        "--minimal",
    ];
    let mut baseline_args = vec!["connect"];
    baseline_args.extend(settings);
    let baseline = request(&baseline_args);
    for command in ["connect", "device"] {
        for toggle in ["on", "off"] {
            for json_output in [false, true] {
                let mut args = vec![command];
                args.extend(settings);
                args.extend(["--sessions", "4", "--cloud-sessions", toggle]);
                if json_output {
                    args.push("--json");
                }
                let mut recorder = Recorder::default();
                let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
                assert_eq!(
                    execute(
                        parse(&args).unwrap(),
                        &mut recorder,
                        &mut stdout,
                        &mut stderr
                    )
                    .unwrap(),
                    0
                );
                assert_eq!(recorder.0, vec![baseline.clone()]);
                assert!(stderr.is_empty());
                if json_output {
                    assert_eq!(
                        serde_json::from_slice::<Value>(&stdout).unwrap(),
                        json!({"connected":true})
                    );
                } else {
                    assert_eq!(stdout, b"connected\n");
                }
            }
        }
    }
}

#[test]
fn product_parser_rejects_missing_arguments_and_invalid_toggles_before_dispatch() {
    // Product parser assertions, not observed compatibility diagnostics.
    for command in ["connect", "device"] {
        for suffix in [
            vec!["--sessions"],
            vec!["--cloud-sessions"],
            vec!["--cloud-sessions", "other"],
            vec!["--cloud-sessions", "ON"],
        ] {
            let mut args = vec![command];
            args.extend(suffix);
            let mut recorder = Recorder::default();
            match parse(&args) {
                Err(_) => {}
                Ok(cli) => {
                    let _ = dispatch(cli, &mut recorder);
                    panic!("expected product parser rejection: {args:?}");
                }
            }
            assert!(recorder.0.is_empty());
        }
    }
}

#[test]
fn product_opaque_tokens_have_no_numeric_conversion_or_effective_capacity() {
    // Characterizes the selected product representation, not numeric conformance.
    let huge = "9".repeat(1024);
    for command in ["connect", "device"] {
        let baseline = request(&[command]);
        for token in [
            "1",
            "4",
            "0",
            "5",
            "0001",
            "arbitrary",
            "1.5",
            "1e9",
            "+1",
            "١",
            " ",
            huge.as_str(),
        ] {
            let cli = parse(&[command, "--sessions", token]).unwrap();
            let Some(Command::Connect(options)) = &cli.command else {
                panic!("connect route")
            };
            assert_eq!(options.sessions.as_deref(), Some(token));
            let mut recorder = Recorder::default();
            dispatch(cli, &mut recorder).unwrap();
            assert_eq!(recorder.0, vec![baseline.clone()]);
        }
    }
}

#[test]
fn generated_connect_help_documents_both_deprecated_no_ops() {
    let mut cli = Cli::command();
    let command = cli.find_subcommand_mut("connect").unwrap();
    let help = command.render_long_help().to_string();
    assert!(help.contains("--sessions <N>"));
    assert!(help.contains("--cloud-sessions"));
    assert!(help.contains("on, off"));
    assert_eq!(help.matches("Deprecated; accepted but ignored").count(), 2);
}
