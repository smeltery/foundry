#![cfg(unix)]
#[path = "granted_execution/support.rs"]
mod support;
use foundry_store::{Store, ToolOutcome};
use serde_json::Value;
use support::*;

#[test]
#[ignore = "requires freshly built f5y-toolhost via FOUNDRY_TEST_TOOLHOST"]
fn granted_cli_write_uses_effective_directory_and_commits_receipt() {
    let fixture = Fixture::new("write");
    let output = fixture.task(&helper(), &["write.create", "write.create"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body = response(&output);
    assert_eq!(body["execution"], "finished");
    assert_eq!(
        std::fs::read_to_string(fixture.root().join("work/out.txt")).unwrap(),
        "owned output"
    );
    assert!(!fixture.root().join("out.txt").exists());
    let store = Store::open(fixture.root().join("state.db")).unwrap();
    let call = store
        .tool_call(body["session"]["id"].as_str().unwrap(), "call")
        .unwrap()
        .unwrap();
    assert_eq!(call.outcome, Some(ToolOutcome::Success));
    assert!(call.output_digest.is_some());

    // Installed layout uses a sibling helper even when PATH cannot locate it.
    let installed = Fixture::new("write");
    let bin = installed.root().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_f5y"), bin.join("f5y")).unwrap();
    std::fs::copy(helper(), bin.join("f5y-toolhost")).unwrap();
    let output = std::process::Command::new(bin.join("f5y"))
        .current_dir(installed.root().join("work"))
        .env_remove("FOUNDRY_TOOLHOST")
        .env_remove("FOUNDRY_DATABASE")
        .env("FOUNDRY_ADAPTER", &installed.adapter)
        .env("PATH", "/nonexistent")
        .args([
            "fixture",
            "--tool-grant",
            "write.create",
            "--database",
            "state.db",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    response(&output);
    assert_eq!(
        std::fs::read_to_string(installed.root().join("work/out.txt")).unwrap(),
        "owned output"
    );

    for form in [
        vec!["run", "--task", "fixture"],
        vec!["session", "start", "--prompt", "fixture"],
    ] {
        let fixture = Fixture::new("write");
        let output = fixture
            .command()
            .current_dir(fixture.root().join("work"))
            .env("FOUNDRY_TOOLHOST", helper())
            .args(form)
            .args(["--tool-grant", "write.create"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        response(&output);
        assert_eq!(
            std::fs::read_to_string(fixture.root().join("work/out.txt")).unwrap(),
            "owned output"
        );
    }
}

#[test]
#[ignore = "requires freshly built f5y-toolhost via FOUNDRY_TEST_TOOLHOST"]
fn read_list_and_overwrite_grants_are_independent() {
    for (mode, grant) in [
        ("read", "read"),
        ("list", "list"),
        ("overwrite", "write.overwrite"),
    ] {
        let fixture = Fixture::new(mode);
        std::fs::write(fixture.root().join("work/input.txt"), "hello").unwrap();
        std::fs::write(fixture.root().join("work/out.txt"), "previous").unwrap();
        let output = fixture.task(&helper(), &[grant]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let body = response(&output);
        let result: Value =
            serde_json::from_str(body["session"]["result"].as_str().unwrap()).unwrap();
        assert_eq!(result["status"], "ok");
        if mode == "read" {
            assert_eq!(result["output"]["content"], "hello");
        }
        if mode == "list" {
            assert_eq!(result["output"]["entries"].as_array().unwrap().len(), 2);
        }
        if mode == "overwrite" {
            assert_eq!(
                std::fs::read_to_string(fixture.root().join("work/out.txt")).unwrap(),
                "owned output"
            );
        }
    }
    for grant in ["read", "list", "write.create"] {
        let fixture = Fixture::new("overwrite");
        std::fs::write(fixture.root().join("work/out.txt"), "previous").unwrap();
        let output = fixture.task(&helper(), &[grant]);
        assert_eq!(output.status.code(), Some(3));
        response(&output);
        assert_eq!(
            std::fs::read_to_string(fixture.root().join("work/out.txt")).unwrap(),
            "previous"
        );
    }
}

#[test]
fn absent_grants_and_yes_keep_default_denial_and_ignore_helper_override() {
    for yes in [false, true] {
        let fixture = Fixture::new("write");
        let mut command = fixture.command();
        command
            .env("FOUNDRY_TOOLHOST", "relative-unused-helper")
            .args(["fixture", "--dir", "work"]);
        if yes {
            command.arg("--yes");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(3));
        response(&output);
        assert!(!fixture.root().join("work/out.txt").exists());
    }
}

#[test]
fn invalid_helper_selection_fails_before_database_creation() {
    let fixture = Fixture::new("write");
    for helper in [
        std::path::PathBuf::from("relative"),
        fixture.root().join("missing"),
        fixture.root().join("work"),
    ] {
        let output = fixture.task(&helper, &["write.create"]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "invalid");
        assert!(!fixture.root().join("state.db").exists());
    }
}

#[test]
fn helper_discovery_never_falls_back_to_path() {
    let fixture = Fixture::new("write");
    let bin = fixture.root().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_f5y"), bin.join("f5y")).unwrap();
    // A similarly named executable on PATH cannot replace a missing sibling.
    std::fs::copy(&fixture.adapter, fixture.root().join("f5y-toolhost")).unwrap();
    let output = std::process::Command::new(bin.join("f5y"))
        .current_dir(fixture.root())
        .env_remove("FOUNDRY_TOOLHOST")
        .env_remove("FOUNDRY_DATABASE")
        .env("FOUNDRY_ADAPTER", &fixture.adapter)
        .env("PATH", fixture.root())
        .args([
            "fixture",
            "--tool-grant",
            "write.create",
            "--database",
            "state.db",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "invalid");
    assert!(output.stdout.is_empty());
    assert!(!fixture.root().join("state.db").exists());
}
