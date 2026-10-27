use serde_json::Value;
use std::process::{Command, Output};
use tempfile::TempDir;

fn run(directory: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_f5y"))
        .current_dir(directory.path())
        .env(
            "FOUNDRY_DATABASE",
            directory.path().join("must-not-exist.db"),
        )
        .env("FOUNDRY_ADAPTER", "invalid-relative-path")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn parser_errors_use_requested_json_without_opening_database() {
    let directory = TempDir::new().unwrap();
    for (args, code) in [
        (vec!["send", "session", " ", "--json"], "invalid"),
        (vec!["--json", "send", "session", " "], "invalid"),
        (
            vec!["send", "session", " ", "--output-format", "json"],
            "invalid",
        ),
        (
            vec!["send", "session", " ", "--output-format=json"],
            "invalid",
        ),
        (vec!["session", "get", "--json"], "usage"),
        (vec!["--unknown", "--json"], "usage"),
        (vec!["--json", "--output-format", "text"], "usage"),
        (vec!["--output-format", "invalid", "--json"], "invalid"),
    ] {
        let output = run(&directory, &args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], code, "{args:?}");
        assert!(!error["error"]["message"].as_str().unwrap().is_empty());
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn positional_or_similar_spellings_do_not_enable_json_errors() {
    let directory = TempDir::new().unwrap();
    for args in [
        vec!["send", "session", " ", "--", "--json"],
        vec!["send", "session", " ", "--", "--output-format", "json"],
        vec!["--jsonish"],
        vec!["--database=--json", "--unknown"],
        vec!["--output-format=text", "--unknown"],
        vec!["--output", "json", "--unknown"],
    ] {
        let output = run(&directory, &args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        assert!(
            serde_json::from_slice::<Value>(&output.stderr).is_err(),
            "{args:?}"
        );
        assert!(String::from_utf8_lossy(&output.stderr).starts_with("error:"));
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn help_and_version_preserve_stdout_and_bypass_configuration() {
    let directory = TempDir::new().unwrap();
    for args in [
        vec!["--json", "--help"],
        vec!["--output-format", "json", "--version"],
        vec!["session", "--help", "--json"],
        vec!["help"],
        vec!["version", "--json"],
    ] {
        let output = run(&directory, &args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}
