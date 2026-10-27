use clap::Parser;
use f5y::{Backend, Request, args::Cli, dispatch, local::LocalBackend};
use foundry_contracts::{CancelMode, CreateSession, Lifecycle};
use foundry_store::Store;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Database(PathBuf);
impl Database {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "f5y-local-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> PathBuf {
        self.0.join("sessions.db")
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn create() -> CreateSession {
    CreateSession {
        prompt: "fixture".into(),
        directory: "/tmp".into(),
        model: None,
        idempotency_key: Some("stable-key".into()),
    }
}
fn cli(backend: &mut LocalBackend, args: &[&str]) -> Result<Value, f5y::CommandError> {
    dispatch(Cli::try_parse_from(args).unwrap(), backend).map(|response| response.json)
}

#[test]
fn durable_create_list_get_and_snapshot() {
    let db = Database::new();
    let id = {
        let mut backend = LocalBackend::new(db.path());
        assert!(!db.path().exists());
        let created = backend.create(create()).unwrap();
        assert_eq!(created["execution"], "not_started");
        assert_eq!(created["session"]["lifecycle"], "queued");
        let same = backend.create(create()).unwrap();
        assert_eq!(created["session"]["id"], same["session"]["id"]);
        created["session"]["id"].as_str().unwrap().to_owned()
    };
    let mut backend = LocalBackend::new(db.path());
    assert_eq!(
        cli(&mut backend, &["f5y", "ls", "--limit", "1"]).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        cli(&mut backend, &["f5y", "session", "get", &id]).unwrap()["session"]["id"],
        id
    );
    assert_eq!(backend.watch_snapshot(&id).unwrap()["watch"], "snapshot");
    let stopped = cli(&mut backend, &["f5y", "session", "cancel", &id, "--hard"]).unwrap();
    assert_eq!(stopped["session"]["lifecycle"], "suspended");
    assert_eq!(stopped["cancellation"], "requested");
}

#[test]
fn start_is_explicitly_a_local_queued_record() {
    let db = Database::new();
    let mut backend = LocalBackend::new(db.path());
    let value = cli(
        &mut backend,
        &["f5y", "session", "start", "--prompt", "fixture"],
    )
    .unwrap();
    assert_eq!(value["target"], "local");
    assert_eq!(value["execution"], "not_started");
    assert_eq!(value["session"]["lifecycle"], "queued");
}

#[test]
fn unsupported_options_and_commands_do_not_open_database() {
    let db = Database::new();
    let mut backend = LocalBackend::new(db.path());
    let cases: &[&[&str]] = &[
        &["f5y", "run", "--task", "fixture"],
        &["f5y", "fixture"],
        &[
            "f5y", "session", "start", "--prompt", "fixture", "--repo", "a/b",
        ],
        &[
            "f5y", "session", "start", "--prompt", "fixture", "--model", "model",
        ],
        &[
            "f5y",
            "session",
            "start",
            "--prompt",
            "fixture",
            "--provider",
            "github",
        ],
        &["f5y", "ls", "--status", "all"],
        &["f5y", "ls", "--fields", "id"],
        &["f5y", "ls", "--origin", "ui"],
        &["f5y", "watch", "session", "--interval", "1"],
        &["f5y", "watch", "session"],
        &["f5y", "session", "watch", "session"],
    ];
    for args in cases {
        let error = cli(&mut backend, args).expect_err("unsupported command");
        assert_eq!(error.code, "unsupported", "{args:?}");
        assert!(!db.path().exists(), "{args:?}");
    }
}

#[test]
fn validation_and_error_codes() {
    let db = Database::new();
    let mut backend = LocalBackend::new(db.path());
    assert_eq!(backend.list(0).unwrap_err().code, "invalid");
    assert_eq!(backend.get(" ").unwrap_err().code, "invalid");
    let mut invalid = create();
    invalid.prompt.clear();
    assert_eq!(backend.create(invalid).unwrap_err().code, "invalid");
    let request = Request {
        version: 2,
        command: json!({"operation":"ls"}),
    };
    assert_eq!(backend.execute(request).err().unwrap().code, "unsupported");
    assert!(!db.path().exists());
    assert_eq!(backend.get("missing").unwrap_err().code, "not_found");
    backend.create(create()).unwrap();
    let mut conflicting = create();
    conflicting.prompt = "different".into();
    assert_eq!(backend.create(conflicting).unwrap_err().code, "conflict");
}

#[test]
fn active_cancel_does_not_claim_process_stopped() {
    let db = Database::new();
    let mut store = Store::open(db.path()).unwrap();
    let session = store.create(create()).unwrap();
    let fence = store.begin_attempt(&session.id, session.version).unwrap();
    let mut backend = LocalBackend::new(db.path());
    let value = backend.cancel(&session.id, CancelMode::Hard).unwrap();
    assert_eq!(value["session"]["lifecycle"], "running");
    assert_eq!(store.cancellation(&fence).unwrap(), Some(CancelMode::Hard));
    assert_eq!(
        store.get(&session.id).unwrap().lifecycle,
        Lifecycle::Running
    );
}

#[test]
fn mcp_adapter_create_get_list_cancel_and_invalid_send() {
    use foundry_mcp::Backend as McpBackend;
    let db = Database::new();
    let mut backend = LocalBackend::new(db.path());
    assert_eq!(
        backend.call("session_send", json!({})).unwrap_err().code,
        "invalid"
    );
    assert!(!db.path().exists());
    assert_eq!(
        backend
            .call(
                "session_create",
                json!({"prompt":"x","directory":"/tmp","extra":true})
            )
            .unwrap_err()
            .code,
        "unsupported"
    );
    assert!(!db.path().exists());
    let created = backend
        .call("session_create", json!({"prompt":"x","directory":"/tmp"}))
        .unwrap();
    let id = created["session"]["id"].clone();
    assert_eq!(
        backend
            .call("session_get", json!({"session_id":id}))
            .unwrap()["session"]["id"],
        id
    );
    assert_eq!(
        backend.call("session_list", json!({})).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        backend
            .call("session_cancel", json!({"session_id":id,"mode":"graceful"}))
            .unwrap()["session"]["lifecycle"],
        "suspended"
    );
}

#[test]
fn mcp_send_retries_preserve_claiming_ambiguity_and_reject_conflicts() {
    use foundry_mcp::Backend as McpBackend;
    let db = Database::new();
    let mut backend = LocalBackend::new(db.path());
    let session = backend.create(create()).unwrap();
    let id = session["session"]["id"].as_str().unwrap();
    let args = json!({"session_id":id,"message_id":"stable-message","message":"follow up"});
    let first = backend.call("session_send", args.clone()).unwrap();
    assert_eq!(first["message"]["state"], "pending");
    assert_eq!(first["delivery_not_confirmed"], true);
    assert_eq!(backend.call("session_send", args.clone()).unwrap(), first);
    let mut changed = args.clone();
    changed["message"] = json!("different");
    assert_eq!(
        backend.call("session_send", changed).unwrap_err().code,
        "conflict"
    );
    let mut store = Store::open(db.path()).unwrap();
    assert_eq!(store.messages(id, 0, 10).unwrap().len(), 1);
    assert_eq!(store.get(id).unwrap().lifecycle, Lifecycle::Queued);
    let fence = store.begin_attempt(id, 0).unwrap();
    store.claim_next_message(&fence).unwrap().unwrap();
    let retry = backend.call("session_send", args.clone()).unwrap();
    assert_eq!(retry["message"]["state"], "in_flight");
    assert_eq!(retry["delivery_not_confirmed"], true);
    store.acknowledge_message(&fence, "stable-message").unwrap();
    let retry = backend.call("session_send", args).unwrap();
    assert_eq!(retry["message"]["state"], "delivered");
    assert_eq!(retry["delivery_not_confirmed"], false);
    assert_eq!(retry["completion"], "not_confirmed");
}

#[test]
fn send_validates_before_opening_database() {
    use foundry_mcp::Backend as McpBackend;
    let db = Database::new();
    let mut backend = LocalBackend::new(db.path());
    for (id, message_id, text) in [("", "m", "x"), ("s", " ", "x"), ("s", "m", " ")] {
        assert_eq!(
            backend.send(id, message_id, text).unwrap_err().code,
            "invalid"
        );
        assert!(!db.path().exists());
    }
    let oversized = "x".repeat(foundry_contracts::MAX_PROMPT_BYTES + 1);
    assert_eq!(
        backend.send("s", "m", &oversized).unwrap_err().code,
        "invalid"
    );
    for args in [
        json!({"session_id":"s","message":"x"}),
        json!({"session_id":"s","message_id":3,"message":"x"}),
    ] {
        assert_eq!(
            backend.call("session_send", args).unwrap_err().code,
            "invalid"
        );
    }
    assert!(!db.path().exists());
}

#[test]
fn withdrawn_retry_is_not_requeued() {
    let db = Database::new();
    let mut backend = LocalBackend::new(db.path());
    let session = backend.create(create()).unwrap();
    let id = session["session"]["id"].as_str().unwrap();
    backend.send(id, "m", "text").unwrap();
    Store::open(db.path())
        .unwrap()
        .withdraw_message(id, "m")
        .unwrap();
    let retry = backend.send(id, "m", "text").unwrap();
    assert_eq!(retry["message"]["state"], "withdrawn");
    assert_eq!(retry["delivery_not_confirmed"], true);
}
