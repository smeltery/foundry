use super::super::super::super::*;
use crate::{
    daemon_config::{DaemonConfig, NetworkMode},
    daemon_host::{Daemon, network::EngineAccess},
    peer_session::{PeerSession, PublicBootstrap, SessionOptions},
    team::JoinRequest,
};
use serde_json::json;
use std::{fs, os::unix::fs::PermissionsExt};

pub(super) struct Node {
    pub root: PathBuf,
    pub state: Arc<State>,
    _daemon: Daemon,
}
impl Node {
    pub async fn open(person: &str) -> Self {
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let root = PathBuf::from("/tmp").join(format!("queue-{:016x}", u64::from_le_bytes(nonce)));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let root = root.canonicalize().unwrap();
        let value = json!({"state_directory":root,"socket_directory":root,"person":person,"machine":"desk","folders":[],"network":"disabled"});
        let daemon = Daemon::open(serde_json::from_value::<DaemonConfig>(value.clone()).unwrap())
            .await
            .unwrap();
        let mut config = value;
        config["network"] = json!({"mode":"enabled","profile":{"team_alpn":[113,116],"join_alpn":[113,106],"protocol":31,"signing_prefix":"receipt-turn/","invitation_endpoint_context":"receipt turn endpoint","invitation_secret_context":"receipt turn secret","ticket_prefixes":["turn31"]},"transport":{"bind_address":"127.0.0.1:0","direct_paths":true,"relays":[],"relay_root_certificates":[]},"seeds":[],"trust":{"mode":"unverified"},"codebook":null,"startup":{"mode":"existing"}});
        let parsed: DaemonConfig = serde_json::from_value(config).unwrap();
        let settings = Arc::new(match parsed.network {
            NetworkMode::Enabled(network) => network.resolve(&root).unwrap(),
            _ => unreachable!(),
        });
        let engine = EngineAccess::for_test(&daemon);
        let path = root.clone();
        engine
            .with(move |engine| {
                use crate::daemon_engine::{ConnectionId, ReplyResult, Request, RequestEnvelope};
                let reply = engine.handle(
                    ConnectionId(1),
                    RequestEnvelope {
                        id: 1,
                        request: Request::Register {
                            name: "terminal".into(),
                            repository: path,
                            share_path: false,
                        },
                    },
                );
                assert!(matches!(reply.result, ReplyResult::Ok(_)));
                Ok(())
            })
            .await
            .unwrap();
        let (authority, identity) = bootstrap::open(&root, engine.clone(), settings.clone())
            .await
            .unwrap();
        let endpoint = Arc::new(
            NetworkEndpoint::bind(settings.endpoint_config(identity.secret_key().clone()))
                .await
                .unwrap(),
        );
        let (stop, _) = watch::channel(false);
        let (connect, _) = tokio::sync::mpsc::channel(32);
        let state = Arc::new(State {
            endpoint,
            authority,
            settings,
            engine,
            addresses: RwLock::new(BTreeMap::new()),
            wake: Notify::new(),
            stop,
            controls: Arc::new(Semaphore::new(32)),
            person: person.into(),
            machine: "desk".into(),
            root: root.clone(),
            codes: codes::Codes::default(),
            connections: connections::Connections::default(),
            connect,
        });
        Self {
            root,
            state,
            _daemon: daemon,
        }
    }
    pub async fn close(self) {
        self.state.endpoint.close().await;
        let root = self.root.clone();
        drop(self);
        fs::remove_dir_all(root).unwrap();
    }
}
pub(super) async fn admit(a: &Node, b: &Node) {
    if matches!(
        a.state.authority.status().await.unwrap(),
        crate::peer_session::AuthorityStatus::Unjoined
    ) {
        drop(
            a.state
                .authority
                .form("receipt-turn".into(), a.state.person.clone(), "desk".into())
                .await
                .unwrap(),
        );
    }
    let publication = a.state.authority.team_publication().await.unwrap();
    let team = publication.state().id.clone();
    let founder = publication.state().founder;
    drop(publication);
    let secret = format!(
        "receipt-invitation-{}",
        iroh::SecretKey::generate().public()
    );
    drop(
        a.state
            .authority
            .issue_invite(secret.clone(), 1, false)
            .await
            .unwrap(),
    );
    let admission = a
        .state
        .authority
        .admit_join(
            b.state.endpoint.id(),
            JoinRequest {
                team: team.clone(),
                secret,
                person: b.state.person.clone(),
                machine: "desk".into(),
                proof: None,
                owner_only: false,
            },
        )
        .await
        .unwrap();
    b.state
        .authority
        .join_existing(PublicBootstrap {
            expected_team: team,
            expected_founder: Some(founder),
            authenticated_inviter: a.state.endpoint.id(),
            state: admission.state().clone(),
        })
        .await
        .unwrap();
    drop(admission);
}
pub(super) async fn pair(
    a: &Node,
    b: &Node,
    cancel: &watch::Sender<bool>,
) -> (PeerSession, PeerSession) {
    let options = || SessionOptions {
        profile: a.state.settings.profile.clone(),
        deadline: DEADLINE,
        cancel: cancel.subscribe(),
    };
    let (a, b) = tokio::join!(
        PeerSession::connect(
            &a.state.endpoint,
            b.state.endpoint.address(),
            a.state.authority.clone(),
            options()
        ),
        PeerSession::accept(&b.state.endpoint, b.state.authority.clone(), options())
    );
    (a.unwrap(), b.unwrap())
}
