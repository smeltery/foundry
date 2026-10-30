#![cfg(unix)]
use foundry_network::{
    daemon_engine::*,
    mail_store::{MailStore, State},
    session_registry::FolderPolicy,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "engine-test-{}",
            data_encoding::HEXLOWER.encode(&nonce)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn open(root: &Root, time: &Arc<AtomicI64>) -> Engine {
    let clock = time.clone();
    Engine::open_with_clock(
        &root.0,
        FolderPolicy::default(),
        TrustedIdentity {
            person: "person".into(),
            machine: "machine".into(),
        },
        move || Ok(clock.load(Ordering::SeqCst)),
    )
    .unwrap()
}
fn handle(engine: &mut Engine, connection: u64, request: Request) -> ReplyResult {
    let reply = engine.handle(
        ConnectionId(connection),
        RequestEnvelope { id: 934, request },
    );
    assert_eq!(reply.id, 934);
    reply.result
}
fn ok(engine: &mut Engine, connection: u64, request: Request) -> Outcome {
    match handle(engine, connection, request) {
        ReplyResult::Ok(value) => value,
        ReplyResult::Error(error) => panic!("{error}"),
    }
}
fn register(engine: &mut Engine, connection: u64, name: &str, path: &Path) -> Auth {
    match ok(
        engine,
        connection,
        Request::Register {
            name: name.into(),
            repository: path.into(),
            share_path: false,
        },
    ) {
        Outcome::Registered { auth } => auth,
        _ => panic!("unexpected register outcome"),
    }
}
fn send(auth: &Auth, id: &str, name: &str, body: &str) -> Request {
    Request::Send {
        auth: auth.clone(),
        message_id: id.into(),
        to: Filter {
            name: Some(name.into()),
            ..Filter::default()
        },
        body: body.into(),
        reply_to: None,
        wake: true,
        notify: false,
    }
}
fn inbox(engine: &mut Engine, conn: u64, auth: &Auth, peek: bool) -> Option<Mail> {
    match ok(
        engine,
        conn,
        Request::Inbox {
            wait_secs: 0,
            auth: auth.clone(),
            peek,
        },
    ) {
        Outcome::Inbox { messages, text } => {
            assert!(messages.len() <= 1);
            assert_eq!(messages.is_empty(), text.is_empty());
            messages.into_iter().next()
        }
        _ => panic!("unexpected inbox outcome"),
    }
}
fn unread(engine: &mut Engine, conn: u64, auth: &Auth) -> u64 {
    match ok(engine, conn, Request::Unread { auth: auth.clone() }) {
        Outcome::Unread { count } => count,
        _ => panic!("unexpected unread outcome"),
    }
}
#[test]
fn flat_json_defaults_unknown_fields_and_correlated_errors() {
    let value: RequestEnvelope =
        serde_json::from_str(r#"{"id":81,"op":"register","name":"s","repository":"/tmp"}"#)
            .unwrap();
    assert_eq!(value.id, 81);
    assert!(matches!(
        value.request,
        Request::Register {
            share_path: false,
            ..
        }
    ));
    for input in [
        r#"{"id":1,"op":"made_up"}"#,
        r#"{"id":1,"op":"register","name":"s","repository":"/tmp","person":"forged"}"#,
        r#"{"id":1,"op":"linked","connection":9}"#,
        r#"{"id":1,"op":"join"}"#,
    ] {
        assert!(serde_json::from_str::<RequestEnvelope>(input).is_err());
    }
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "a", &root.0);
    let mut json = serde_json::to_value(send(&auth, "m", "a", "text")).unwrap();
    json.as_object_mut().unwrap().remove("wake");
    json.as_object_mut().unwrap().remove("notify");
    let request: Request = serde_json::from_value(json).unwrap();
    assert!(matches!(
        request,
        Request::Send {
            wake: true,
            notify: false,
            ..
        }
    ));
    let mut json = serde_json::to_value(Request::Inbox {
        wait_secs: 0,
        auth: auth.clone(),
        peek: true,
    })
    .unwrap();
    json.as_object_mut().unwrap().remove("peek");
    json.as_object_mut().unwrap().remove("wait_secs");
    assert!(matches!(
        serde_json::from_value::<Request>(json).unwrap(),
        Request::Inbox {
            wait_secs: 0,
            peek: false,
            ..
        }
    ));
    assert!(matches!(
        handle(&mut engine, 2, Request::Unread { auth }),
        ReplyResult::Error(EngineError::Unauthorized)
    ));
}
#[test]
fn real_send_peek_claim_ack_and_exact_replay() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let alice = register(&mut engine, 1, "alice", &root.0);
    let bob = register(&mut engine, 2, "bob", &root.0);
    assert!(matches!(
        ok(&mut engine, 1, send(&alice, "m", "bob", "hello")),
        Outcome::Sent {
            duplicate: false,
            state: MailState::Pending,
            ..
        }
    ));
    assert!(matches!(
        ok(&mut engine, 1, send(&alice, "m", "bob", "hello")),
        Outcome::Sent {
            duplicate: true,
            state: MailState::Pending,
            ..
        }
    ));
    assert!(matches!(
        handle(&mut engine, 1, send(&alice, "m", "bob", "changed")),
        ReplyResult::Error(EngineError::Conflict)
    ));
    let peek = inbox(&mut engine, 2, &bob, true).unwrap();
    assert!(peek.attempt_id.is_some());
    assert_eq!(
        data_encoding::BASE64
            .decode(peek.body_base64.as_bytes())
            .unwrap(),
        b"hello"
    );
    assert!(peek.wake);
    assert!(!peek.notify);
    let mail = inbox(&mut engine, 2, &bob, true).unwrap();
    let id = mail.attempt_id.unwrap();
    assert_eq!(
        inbox(&mut engine, 2, &bob, true).unwrap().attempt_id,
        Some(id)
    );
    assert_eq!(unread(&mut engine, 2, &bob), 1);
    ok(
        &mut engine,
        2,
        Request::Delivered {
            auth: bob.clone(),
            attempt_id: id,
        },
    );
    assert_eq!(unread(&mut engine, 2, &bob), 0);
    assert!(matches!(
        handle(
            &mut engine,
            2,
            Request::Delivered {
                auth: bob,
                attempt_id: id
            }
        ),
        ReplyResult::Error(EngineError::StaleAttempt)
    ));
    assert!(matches!(
        ok(&mut engine, 1, send(&alice, "m", "bob", "hello")),
        Outcome::Sent {
            duplicate: true,
            state: MailState::Delivered,
            ..
        }
    ));
    assert_eq!(
        MailStore::open(&root.0).unwrap().get("m").unwrap().state,
        State::Delivered
    );
}
#[path = "../src/daemon_engine/batch_tests.rs"]
mod batch_tests;
#[path = "../src/daemon_engine/behavior_tests.rs"]
mod behavior_tests;
#[path = "../src/daemon_engine/bounds_tests.rs"]
mod bounds_tests;
#[path = "../src/daemon_engine/inbox_tests.rs"]
mod inbox_tests;

#[path = "../src/daemon_engine/output_tests.rs"]
mod output_tests;
#[path = "../src/daemon_engine/quota_tests.rs"]
mod quota_tests;
#[path = "../src/daemon_engine/remote_tests.rs"]
mod remote_tests;
