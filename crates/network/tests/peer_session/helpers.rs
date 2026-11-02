use super::*;
use cap_std::{ambient_authority, fs::Dir};
use foundry_network::{
    LocalIdentity,
    full_team_store::{FullTeamStore, StorePolicy},
    team::TeamDocument,
    team_state::{Admission, Formation},
};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

pub const NOW: i64 = 8_000_000;
pub const WAIT: Duration = Duration::from_secs(5);
pub struct Node {
    pub root: PathBuf,
    pub identity: Arc<LocalIdentity>,
}
impl Node {
    pub fn new() -> Self {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!("peer-{:x}", u64::from_ne_bytes(nonce)));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let dir = Dir::open_ambient_dir(&root, ambient_authority()).unwrap();
        let identity = Arc::new(foundry_network::load_or_create_identity(&dir, WAIT).unwrap());
        Self { root, identity }
    }
    pub fn create(&self) -> FullTeamStore {
        FullTeamStore::create_new(
            &self.root,
            self.identity.clone(),
            StorePolicy::new(profile(), None).unwrap(),
            WAIT,
        )
        .unwrap()
    }
    pub fn store(&self) -> FullTeamStore {
        FullTeamStore::open_existing(
            &self.root,
            self.identity.clone(),
            StorePolicy::new(profile(), None).unwrap(),
            WAIT,
        )
        .unwrap()
    }
    pub async fn endpoint(&self) -> NetworkEndpoint {
        NetworkEndpoint::bind(foundry_network::EndpointConfig {
            profile: profile(),
            secret_key: self.identity.secret_key().clone(),
            relay: foundry_network::RelaySelection::Disabled,
            bind_address: "127.0.0.1:0".parse().unwrap(),
            direct_paths: true,
            relay_root_certificates: Vec::new(),
        })
        .await
        .unwrap()
    }
    pub fn authority(&self, mailbox: bool) -> Arc<AdmittedTeam> {
        let mailbox = mailbox.then(|| {
            TextMailbox::new(
                foundry_network::mail_store::MailStore::open(&self.root).unwrap(),
                |address| {
                    if address.ends_with("/terminal") {
                        Ok(address.into())
                    } else {
                        Err(PeerError::Unauthorized)
                    }
                },
            )
        });
        let directory = Dir::open_ambient_dir(&self.root, ambient_authority()).unwrap();
        let ownership = daemon_lease::DaemonLease::acquire(&directory).unwrap();
        AdmittedTeam::new(
            self.store(),
            ownership,
            self.identity.endpoint_id(),
            || Ok(NOW + 100),
            mailbox,
        )
        .unwrap()
    }
}
impl Drop for Node {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
pub fn profile() -> WireProfile {
    WireProfile {
        team_alpn: b"fixture-peer/team/1".to_vec(),
        join_alpn: b"fixture-peer/join/1".to_vec(),
        protocol: 1,
        signing_prefix: "fixture-peer/sign/".into(),
        invitation_endpoint_context: "fixture endpoint seed".into(),
        invitation_secret_context: "fixture secret seed".into(),
        ticket_prefixes: vec!["fp1-".into()],
    }
}
pub fn options(cancel: &watch::Sender<bool>) -> SessionOptions {
    SessionOptions {
        profile: profile(),
        deadline: WAIT,
        cancel: cancel.subscribe(),
    }
}
pub fn form(node: &Node, person: &str) -> FullTeamStore {
    let mut store = node.create();
    let base = store.load(NOW).unwrap();
    let pending = store
        .prepare_formation(
            &base,
            Formation {
                name: "fixture",
                person,
                machine: "desktop",
            },
            NOW,
        )
        .unwrap();
    store.commit_formation(pending).unwrap();
    store
}
pub fn provision(left: &Node, right: &Node) {
    let mut store = form(left, "alice");
    let base = store.load(NOW).unwrap();
    let invite = store
        .prepare_invite(&base, "fixture-access", 1, false, NOW)
        .unwrap();
    store.commit_transition(invite, NOW).unwrap();
    let base = store.load(NOW).unwrap();
    let admission = store
        .prepare_admission(
            &base,
            Admission {
                endpoint: right.identity.endpoint_id(),
                person: "bob",
                machine: "desktop",
                secret: "fixture-access",
                proof: None,
                owner_only: false,
            },
            NOW,
        )
        .unwrap();
    store.commit_transition(admission, NOW).unwrap();
    let state = store.load(NOW).unwrap();
    join_store(
        right,
        state.document().unwrap().clone(),
        &state.document().unwrap().id,
        left.identity.endpoint_id(),
        left.identity.endpoint_id(),
    );
}

// Real store bootstrap with independently supplied formation trust. No schema
// fixtures or local checkpoint/signature writer are needed.
fn join_store(
    node: &Node,
    document: TeamDocument,
    team: &str,
    founder: iroh::EndpointId,
    inviter: iroh::EndpointId,
) {
    let mut store = node.create();
    let base = store.load(NOW).unwrap();
    let pending = store
        .prepare_join(
            &base,
            &document,
            full_team_store::JoinTrust {
                team,
                founder: Some(founder),
                inviter,
            },
            NOW,
        )
        .unwrap();
    store.commit_join(pending).unwrap();
}
pub async fn pair(
    left: &NetworkEndpoint,
    right: &NetworkEndpoint,
    a: Arc<AdmittedTeam>,
    b: Arc<AdmittedTeam>,
    cancel: &watch::Sender<bool>,
) -> (PeerSession, PeerSession) {
    let (a, b) = tokio::join!(
        PeerSession::connect(left, right.address(), a, options(cancel)),
        PeerSession::accept(right, b, options(cancel))
    );
    (a.unwrap(), b.unwrap())
}
pub fn message() -> wire::Message {
    wire::Message {
        id: "receipt-1".into(),
        from: "alice/desktop/terminal".into(),
        from_agent: None,
        from_repo: None,
        to: "bob/desktop/terminal".into(),
        via: None,
        reply_to: Some("threadstart".into()),
        body: "independent fixture text".into(),
        wake: true,
        notify: false,
        sent_at: NOW,
        approval: None,
        files: Vec::new(),
    }
}
pub async fn raw(
    left: &NetworkEndpoint,
    right: &NetworkEndpoint,
    b: Arc<AdmittedTeam>,
    cancel: &watch::Sender<bool>,
    hello: PeerFrame,
) -> Result<PeerSession> {
    let client = async {
        let connection = left
            .connect(right.address(), Channel::Team, WAIT)
            .await
            .unwrap();
        let (mut send, mut receive) = connection.open_bi().await.unwrap();
        foundry_network::write_frame(&mut send, &hello, WAIT)
            .await
            .unwrap();
        let _ = foundry_network::read_frame(&mut receive, WAIT).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        connection.close(0u32.into(), b"fixture finished");
    };
    let (_, server) = tokio::join!(client, PeerSession::accept(right, b, options(cancel)));
    server
}

pub struct RawPeer {
    pub connection: iroh::endpoint::Connection,
    pub send: iroh::endpoint::SendStream,
    pub receive: iroh::endpoint::RecvStream,
}
pub async fn raw_pair(
    a: &Node,
    left: &NetworkEndpoint,
    right: &NetworkEndpoint,
    b: Arc<AdmittedTeam>,
    cancel: &watch::Sender<bool>,
) -> (RawPeer, PeerSession) {
    let document = a
        .store()
        .load(NOW + 100)
        .unwrap()
        .document()
        .unwrap()
        .clone();
    let client = async {
        let connection = left
            .connect(right.address(), Channel::Team, WAIT)
            .await
            .unwrap();
        let (mut send, mut receive) = connection.open_bi().await.unwrap();
        write_frame(
            &mut send,
            &PeerFrame::Hello {
                team: document.id.clone(),
                proto: 1,
            },
            WAIT,
        )
        .await
        .unwrap();
        assert!(matches!(
            read_frame(&mut receive, WAIT).await.unwrap(),
            Some(PeerFrame::Hello { .. })
        ));
        let state: wire::StateDocument =
            serde_json::from_value(serde_json::to_value(document).unwrap()).unwrap();
        write_frame(
            &mut send,
            &PeerFrame::State {
                state: Box::new(state),
            },
            WAIT,
        )
        .await
        .unwrap();
        assert!(matches!(
            read_frame(&mut receive, WAIT).await.unwrap(),
            Some(PeerFrame::State { .. })
        ));
        RawPeer {
            connection,
            send,
            receive,
        }
    };
    let (raw, server) = tokio::join!(client, PeerSession::accept(right, b, options(cancel)));
    (raw, server.unwrap())
}
impl Drop for RawPeer {
    fn drop(&mut self) {
        self.connection.close(0u32.into(), b"raw fixture closed");
    }
}

pub fn ownership(node: &Node) -> daemon_lease::DaemonLease {
    daemon_lease::DaemonLease::acquire(
        &Dir::open_ambient_dir(&node.root, ambient_authority()).unwrap(),
    )
    .unwrap()
}
