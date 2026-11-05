//! In-process engine fixture for directory, presence and redaction tests.
use foundry_network::{daemon_engine::*, session_registry::FolderPolicy, wire};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};

pub(crate) const PERSON: &str = "alex";
pub(crate) const MACHINE: &str = "laptop";

/// A private state root and a settable engine clock in epoch milliseconds.
pub(crate) struct Fixture {
    pub(crate) root: PathBuf,
    pub(crate) clock: Arc<AtomicI64>,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!(
            "engine-directory-{}",
            data_encoding::HEXLOWER.encode(&nonce)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root,
            clock: Arc::new(AtomicI64::new(1_000)),
        }
    }
    pub(crate) fn open(&self) -> Engine {
        let clock = self.clock.clone();
        Engine::open_with_clock(
            &self.root,
            FolderPolicy::default(),
            TrustedIdentity {
                person: PERSON.into(),
                machine: MACHINE.into(),
            },
            move || Ok(clock.load(Ordering::SeqCst)),
        )
        .unwrap()
    }
    pub(crate) fn advance(&self, milliseconds: i64) {
        self.clock.fetch_add(milliseconds, Ordering::SeqCst);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Report, never panic: a second panic would hide the test's own failure.
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            eprintln!("cleanup of {:?} failed: {error}", self.root);
        }
    }
}

pub(crate) fn reply(engine: &mut Engine, connection: u64, request: Request) -> ReplyResult {
    let reply = engine.handle(
        ConnectionId(connection),
        RequestEnvelope { id: 77, request },
    );
    assert_eq!(reply.id, 77);
    reply.result
}
pub(crate) fn ok(engine: &mut Engine, connection: u64, request: Request) -> Outcome {
    match reply(engine, connection, request) {
        ReplyResult::Ok(value) => value,
        ReplyResult::Error(error) => panic!("{error}"),
    }
}
pub(crate) fn register(engine: &mut Engine, connection: u64, name: &str, root: &Path) -> Auth {
    match ok(
        engine,
        connection,
        Request::Register {
            name: name.into(),
            repository: root.into(),
            share_path: false,
        },
    ) {
        Outcome::Registered { auth } => auth,
        _ => panic!("unexpected registration outcome"),
    }
}
pub(crate) fn send_to(auth: &Auth, id: &str, to: Filter) -> Request {
    Request::Send {
        auth: auth.clone(),
        message_id: id.into(),
        to,
        body: format!("body of {id}"),
        reply_to: None,
        wake: true,
        notify: false,
    }
}
pub(crate) fn directory(engine: &mut Engine, connection: u64, filter: Filter) -> Vec<Entry> {
    match ok(engine, connection, Request::Who { filter }) {
        Outcome::Directory { entries } => entries,
        _ => panic!("unexpected directory outcome"),
    }
}
pub(crate) fn peer(seed: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[seed; 32]).public()
}
pub(crate) fn member(seed: u8, person: &str, machine: &str) -> RemoteMember {
    RemoteMember {
        peer: peer(seed),
        person: person.into(),
        machine: machine.into(),
    }
}
/// One online pull session advertised by a remote member.
pub(crate) fn advertised(name: &str, doing: Option<&str>) -> wire::Presence {
    wire::Presence {
        sessions: vec![wire::SessionPresence {
            address: format!("asserted/elsewhere/{name}"),
            person: "asserted".into(),
            machine: "elsewhere".into(),
            name: name.into(),
            agent: "client".into(),
            cwd: String::new(),
            repo: None,
            branch: None,
            status: "ready".into(),
            since: None,
            doing: doing.map(Into::into),
            tier: wire::DeliveryTier::Pull,
            unread: 0,
            is_me: true,
        }],
        ..Default::default()
    }
}
