use super::support::*;
use foundry_contracts::CreateSession;
use foundry_store::Store;

#[test]
fn prompt_free_resume_reads_same_session_without_configuration_or_admission() {
    let h = Fixture::new("ok");
    let mut store = Store::open(&h.database).unwrap();
    let before = store
        .create(CreateSession {
            prompt: "original".into(),
            directory: h.work.display().to_string(),
            model: None,
            idempotency_key: None,
        })
        .unwrap();
    std::fs::remove_file(&h.config).unwrap();
    let output = h
        .command()
        .env(
            "FOUNDRY_NATIVE_PROFILE",
            h.root.path().join("missing-registration"),
        )
        .args(["--resume", &before.id])
        .output()
        .unwrap();
    let body = response(&output);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(body["session"], serde_json::to_value(&before).unwrap());
    assert_eq!(
        serde_json::to_value(store.get(&before.id).unwrap()).unwrap(),
        body["session"]
    );
    assert!(store.messages(&before.id, 0, 100).unwrap().is_empty());
    assert!(store.events(&before.id, 0, 100).unwrap().is_empty());
    assert!(!h.work.join("received.json").exists());
}

#[test]
fn prompt_free_resume_missing_session_keeps_storage_error() {
    let h = Fixture::new("ok");
    drop(Store::open(&h.database).unwrap());
    let output = h
        .command()
        .env(
            "FOUNDRY_NATIVE_PROFILE",
            h.root.path().join("missing-registration"),
        )
        .args(["--resume", "absent"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not_found"));
    assert!(!h.work.join("received.json").exists());
    private_output(&output);
}

#[test]
fn resume_never_creates_a_missing_database() {
    for prompt in [None, Some("continue")] {
        let h = Fixture::new("ok");
        std::fs::remove_file(&h.config).unwrap();
        let mut command = h.command();
        command
            .env(
                "FOUNDRY_NATIVE_PROFILE",
                h.root.path().join("missing-registration"),
            )
            .args(["--resume", "absent"]);
        if let Some(prompt) = prompt {
            command.arg(prompt);
        }
        let output = command.output().unwrap();
        h.assert_preflight(&output);
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("database must be an existing file")
        );
    }
}

#[test]
fn native_selection_never_falls_back_without_private_configuration() {
    let h = Fixture::new("ok");
    let output = h
        .command()
        .env_remove("FOUNDRY_ADAPTER_CONFIG")
        .env("FOUNDRY_NATIVE_PROFILE", h.root.path().join("registration"))
        .arg("task")
        .output()
        .unwrap();
    h.assert_preflight(&output);
    // Exercise the shared fixture codec while checking no adapter was reached.
    assert_eq!(h.value()["version"], 1);
}
