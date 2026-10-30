#![cfg(unix)]
mod common;
#[path = "full_team_store_support/mod.rs"]
mod support;
use foundry_network::{
    NetworkEndpoint, RelaySelection,
    code_invite::{CodePolicy, Codebook},
    code_listener::{CodeExit, CodeListener, CodeTransport},
    daemon_lease::DaemonLease,
    full_team_store::FullTeamStore,
    join_protocol::{JoinResult, join_peer},
    peer_session::{AdmittedTeam, PublicCodeBootstrap},
    team::JoinRequest,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use support::{Fixture, NOW, WAIT, formation};
const DEADLINE: Duration = Duration::from_secs(5);

fn codes(f: &Fixture) -> CodePolicy {
    let bytes = b"amber\ncedar\n";
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .try_into()
        .unwrap();
    CodePolicy::new(
        Codebook::from_bytes(bytes, digest).unwrap(),
        &f.base.profile,
    )
    .unwrap()
}
fn authority(f: &Fixture, clock: Arc<AtomicI64>) -> Arc<AdmittedTeam> {
    authority_with_peer(f, clock, None)
}
fn authority_with_peer(
    f: &Fixture,
    clock: Arc<AtomicI64>,
    peer: Option<iroh::EndpointId>,
) -> Arc<AdmittedTeam> {
    let mut store = FullTeamStore::create_new(
        &f.base.root,
        f.identity.clone(),
        f.policy().with_codes(codes(f)).unwrap(),
        WAIT,
    )
    .unwrap();
    let base = store.load(NOW).unwrap();
    let pending = store.prepare_formation(&base, formation(), NOW).unwrap();
    store.commit_formation(pending).unwrap();
    if let Some(peer) = peer {
        let base = store.load(NOW).unwrap();
        let invite = store
            .prepare_invite(&base, "fixture-member", 1, false, NOW)
            .unwrap();
        store.commit_transition(invite, NOW).unwrap();
        let base = store.load(NOW).unwrap();
        let pending = store
            .prepare_admission(
                &base,
                foundry_network::team_state::Admission {
                    endpoint: peer,
                    person: "bob",
                    machine: "laptop",
                    secret: "fixture-member",
                    proof: None,
                    owner_only: false,
                },
                NOW,
            )
            .unwrap();
        store.commit_transition(pending, NOW).unwrap();
    }
    let root =
        cap_std::fs::Dir::open_ambient_dir(&f.base.root, cap_std::ambient_authority()).unwrap();
    let lease = DaemonLease::acquire(&root).unwrap();
    AdmittedTeam::new(
        store,
        lease,
        f.identity.endpoint_id(),
        move || Ok(clock.load(Ordering::Acquire)),
        None,
    )
    .unwrap()
}
fn transport() -> CodeTransport {
    CodeTransport {
        relay: RelaySelection::Disabled,
        bind_address: "127.0.0.1:0".parse().unwrap(),
        direct_paths: true,
        relay_root_certificates: Vec::new(),
        timeout: DEADLINE,
    }
}

#[tokio::test]
async fn recovered_listener_accepts_empty_team_code_join_and_bootstraps_real_store() {
    let (issuer, joiner) = (Fixture::new(), Fixture::new());
    let authority = authority(&issuer, Arc::new(AtomicI64::new(NOW)));
    let issued = authority.issue_code(1, false).await.unwrap();
    let selected = codes(&issuer).normalize(issued.issued().code()).unwrap();
    drop(issued);
    let leases = authority.recover_code_intents().await.unwrap();
    assert_eq!(leases.len(), 1);
    let listener = CodeListener::bind(authority.clone(), leases[0].clone(), transport())
        .await
        .unwrap();
    let publication = listener.publication().await.unwrap();
    let address = publication.address().clone();
    drop(publication);
    let (_stop, cancel) = tokio::sync::watch::channel(false);
    let serving = tokio::spawn(listener.serve(cancel));
    let mut config = common::config();
    config.profile = issuer.base.profile.clone();
    config.secret_key = joiner.identity.secret_key().clone();
    let endpoint = NetworkEndpoint::bind(config).await.unwrap();
    let policy = codes(&issuer);
    let derived = policy.derive(&selected).unwrap();
    let request = JoinRequest {
        team: String::new(),
        secret: derived.bearer_secret().into(),
        person: "bob".into(),
        machine: "laptop".into(),
        proof: None,
        owner_only: false,
    };
    let result = join_peer(&endpoint, address, &request, DEADLINE)
        .await
        .unwrap();
    let JoinResult::Joined(joined) = result else {
        panic!("code invitation refused")
    };
    let anchor = policy
        .authenticate_endpoint(&selected, joined.peer)
        .unwrap();
    let store = FullTeamStore::create_new(
        &joiner.base.root,
        joiner.identity.clone(),
        issuer.policy(),
        WAIT,
    )
    .unwrap();
    let root = cap_std::fs::Dir::open_ambient_dir(&joiner.base.root, cap_std::ambient_authority())
        .unwrap();
    let joined_authority = AdmittedTeam::bootstrap_code(
        store,
        DaemonLease::acquire(&root).unwrap(),
        joiner.identity.endpoint_id(),
        || Ok(NOW),
        None,
        PublicCodeBootstrap {
            anchor,
            expected_team: None,
            expected_founder: None,
            state: joined.state,
        },
    )
    .await
    .unwrap();
    assert!(joined_authority.refresh().await.is_ok());
    assert_eq!(
        tokio::time::timeout(DEADLINE, serving)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        CodeExit::Joined
    );
    assert!(authority.recover_code_intents().await.unwrap().is_empty());
    assert!(
        CodeListener::bind(authority, leases[0].clone(), transport())
            .await
            .is_err()
    );
    endpoint.shutdown().await;
}

#[tokio::test]
async fn publication_permit_blocks_cooperating_removal_and_stale_republication() {
    let f = Fixture::new();
    let target = iroh::SecretKey::generate().public();
    let authority = authority_with_peer(&f, Arc::new(AtomicI64::new(NOW)), Some(target));
    drop(authority.issue_code(1, false).await.unwrap());
    let lease = authority
        .recover_code_intents()
        .await
        .unwrap()
        .pop()
        .unwrap();
    let listener = CodeListener::bind(authority.clone(), lease, transport())
        .await
        .unwrap();
    let publication = listener.publication().await.unwrap();
    let remover = authority.clone();
    let mut removing = tokio::spawn(async move { remover.remove(vec![target]).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut removing)
            .await
            .is_err()
    );
    drop(publication);
    tokio::time::timeout(DEADLINE, removing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(listener.publication().await.is_err());
    listener.shutdown().await;
}

#[tokio::test]
async fn expiry_stops_serving_and_cannot_publish_an_old_lease() {
    let f = Fixture::new();
    let clock = Arc::new(AtomicI64::new(NOW));
    let authority = authority(&f, clock.clone());
    drop(authority.issue_code(1, false).await.unwrap());
    let lease = authority
        .recover_code_intents()
        .await
        .unwrap()
        .pop()
        .unwrap();
    let listener = CodeListener::bind(authority, lease, transport())
        .await
        .unwrap();
    clock.store(NOW + 3_600_000, Ordering::Release);
    assert!(listener.publication().await.is_err());
    let (_stop, cancel) = tokio::sync::watch::channel(false);
    assert_eq!(
        listener.serve(cancel).await.unwrap(),
        CodeExit::RefreshRequired
    );
}

#[tokio::test]
async fn stop_closes_a_partial_join_without_consuming_the_invitation() {
    let f = Fixture::new();
    let authority = authority(&f, Arc::new(AtomicI64::new(NOW)));
    let issued = authority.issue_code(1, false).await.unwrap();
    let policy = codes(&f);
    let code = policy.normalize(issued.issued().code()).unwrap();
    drop(issued);
    let lease = authority
        .recover_code_intents()
        .await
        .unwrap()
        .pop()
        .unwrap();
    let listener = CodeListener::bind(authority.clone(), lease, transport())
        .await
        .unwrap();
    let publication = listener.publication().await.unwrap();
    let address = publication.address().clone();
    drop(publication);
    let (stop, cancel) = tokio::sync::watch::channel(false);
    let serving = tokio::spawn(listener.serve(cancel));
    let mut config = common::config();
    config.profile = f.base.profile.clone();
    let endpoint = NetworkEndpoint::bind(config).await.unwrap();
    let connection = endpoint
        .connect(address, foundry_network::Channel::Join, DEADLINE)
        .await
        .unwrap();
    let (mut send, _receive) = connection.open_bi().await.unwrap();
    let request = JoinRequest {
        team: String::new(),
        secret: policy.derive(&code).unwrap().bearer_secret().into(),
        person: "bob".into(),
        machine: "laptop".into(),
        proof: None,
        owner_only: false,
    };
    let body = serde_json::to_vec(&request).unwrap();
    let prefix = (body.len() as u32).to_be_bytes();
    send.write_all(&prefix[..2]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    stop.send(true).unwrap();
    assert_eq!(
        tokio::time::timeout(DEADLINE, serving)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        CodeExit::Stopped
    );
    tokio::time::timeout(DEADLINE, connection.closed())
        .await
        .unwrap();
    let _ = send.write_all(&[prefix[2..].to_vec(), body].concat()).await;
    assert_eq!(authority.recover_code_intents().await.unwrap().len(), 1);
    endpoint.shutdown().await;
}
