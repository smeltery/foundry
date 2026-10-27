use foundry_contracts::{Lifecycle, MessageState};
use foundry_store::Store;
use serde_json::Value;
use std::{path::Path, process::Command};
use tempfile::TempDir;

fn command(directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_f5y"));
    command
        .current_dir(directory)
        .env_remove("FOUNDRY_DATABASE")
        .env_remove("FOUNDRY_ADAPTER");
    command
}
fn run(directory: &Path, args: &[&str]) -> Value {
    let output = command(directory)
        .args(args)
        .args(["--database", "state.db", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn cli_messages_persist_across_processes_without_resuming_stopped_session() {
    let directory = TempDir::new().unwrap();
    let path = directory.path();
    let session = run(path, &["session", "start", "--prompt", "fixture"]);
    let id = session["session"]["id"].as_str().unwrap();
    let cancelled = run(path, &["cancel", id]);
    assert_eq!(cancelled["session"]["lifecycle"], "suspended");
    let first = run(path, &["send", id, "first instruction"]);
    let second = run(
        path,
        &["session", "send", id, "--message", "second instruction"],
    );
    for value in [&first, &second] {
        assert_eq!(value["message"]["state"], "pending");
        assert_eq!(value["delivery_not_confirmed"], true);
        assert_eq!(value["completion"], "not_confirmed");
        let uuid = uuid::Uuid::parse_str(value["message"]["id"].as_str().unwrap()).unwrap();
        assert_eq!(uuid.get_version_num(), 4);
    }
    assert_ne!(first["message"]["id"], second["message"]["id"]);
    let store = Store::open(path.join("state.db")).unwrap();
    let messages = store.messages(id, 0, 10).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].text, "first instruction");
    assert_eq!(messages[1].text, "second instruction");
    assert_eq!(messages[0].sequence, 1);
    assert_eq!(messages[1].sequence, 2);
    assert!(
        messages
            .iter()
            .all(|message| message.state == MessageState::Pending)
    );
    let session = store.get(id).unwrap();
    assert_eq!(session.lifecycle, Lifecycle::Suspended);
    assert!(session.active_attempt_id.is_none());
    assert!(session.outcome.is_none());
}

#[test]
fn invalid_cli_message_does_not_create_database() {
    let directory = TempDir::new().unwrap();
    let path = directory.path();
    let output = command(path)
        .args(["send", "session", " ", "--database", "state.db", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "invalid");
    assert!(!path.join("state.db").exists());
}
