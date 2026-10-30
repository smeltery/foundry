use super::*;
use cap_std::{ambient_authority, fs::Dir};
use foundry_network::{
    join_protocol::{self, JoinResult, Refusal},
    team::JoinRequest,
};

fn ownership(node: &Node) -> daemon_lease::DaemonLease {
    daemon_lease::DaemonLease::acquire(
        &Dir::open_ambient_dir(&node.root, ambient_authority()).unwrap(),
    )
    .unwrap()
}
fn invitation(a: &Node) -> String {
    let mut store = form(a, "alice");
    let base = store.load(NOW).unwrap();
    let pending = store
        .prepare_invite(&base, "join-fixture-secret", 1, false, NOW)
        .unwrap();
    store.commit_transition(pending, NOW).unwrap();
    store.load(NOW).unwrap().document().unwrap().id.clone()
}
fn request(team: &str) -> JoinRequest {
    JoinRequest {
        team: team.into(),
        secret: "join-fixture-secret".into(),
        person: "bob".into(),
        machine: "desktop".into(),
        proof: None,
        owner_only: false,
    }
}
#[tokio::test]
async fn actual_join_admission_reply_permit_and_public_bootstrap_enable_team_session() {
    let (a, b) = (Node::new(), Node::new());
    let team = invitation(&a);
    let authority = a.authority(false);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let server = async {
        let connection = left.accept(WAIT).await.unwrap();
        let accepted = join_protocol::serve_join(connection, &profile(), WAIT, |peer, request| {
            let authority = authority.clone();
            async move {
                authority
                    .admit_join(peer, request)
                    .await
                    .map_err(|_| Refusal::Denied)
            }
        })
        .await
        .unwrap();
        assert!(accepted);
    };
    let join_request = request(&team);
    let client = join_protocol::join_peer(&right, left.address(), &join_request, WAIT);
    let (_, reply) = tokio::join!(server, client);
    let JoinResult::Joined(joined) = reply.unwrap() else {
        panic!("join refused");
    };
    assert_eq!(joined.peer, left.id());
    let input = PublicBootstrap {
        expected_team: team.clone(),
        expected_founder: None,
        authenticated_inviter: joined.peer,
        state: joined.state,
    };
    let b_authority = AdmittedTeam::bootstrap(
        b.create(),
        ownership(&b),
        b.identity.endpoint_id(),
        || Ok(NOW + 100),
        None,
        input,
    )
    .await
    .unwrap();
    assert!(
        b.store()
            .load(NOW + 100)
            .unwrap()
            .document()
            .unwrap()
            .members[&right.id().to_string()]
            .body
            .active
    );
    let (cancel, _) = watch::channel(false);
    let (mut sender, mut receiver) = pair(&left, &right, authority, b_authority, &cancel).await;
    sender
        .publish_presence(wire::Presence::default())
        .await
        .unwrap();
    assert!(
        matches!(receiver.receive().await.unwrap(), PeerEvent::Presence { peer, .. } if peer == left.id())
    );
    drop((sender, receiver));
    tokio::join!(left.shutdown(), right.shutdown());
}
#[tokio::test]
async fn joined_reply_is_pinned_to_actual_peer_and_holds_gate_until_output_finishes() {
    let (a, b, c) = (Node::new(), Node::new(), Node::new());
    let team = invitation(&a);
    let authority = a.authority(false);
    let permit = authority
        .admit_join(b.identity.endpoint_id(), request(&team))
        .await
        .unwrap();
    assert!(
        permit.state().members[&b.identity.endpoint_id().to_string()]
            .body
            .active
    );
    assert!(
        !permit
            .state()
            .members
            .contains_key(&c.identity.endpoint_id().to_string())
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(40),
            authority.remove(vec![b.identity.endpoint_id()])
        )
        .await
        .is_err()
    );
    drop(permit);
    authority
        .remove(vec![b.identity.endpoint_id()])
        .await
        .unwrap();
    assert!(
        !a.store()
            .load(NOW + 100)
            .unwrap()
            .document()
            .unwrap()
            .members[&b.identity.endpoint_id().to_string()]
            .body
            .active
    );
}
#[tokio::test]
async fn wrong_team_secret_and_public_bootstrap_trust_are_refused_without_reset() {
    let (a, b) = (Node::new(), Node::new());
    let team = invitation(&a);
    let authority = a.authority(false);
    let mut wrong = request(&team);
    wrong.team = "other-team".into();
    assert!(matches!(
        authority.admit_join(b.identity.endpoint_id(), wrong).await,
        Err(PeerError::DifferentTeam)
    ));
    let mut wrong = request(&team);
    wrong.secret = "incorrect".into();
    assert!(matches!(
        authority.admit_join(b.identity.endpoint_id(), wrong).await,
        Err(PeerError::Unauthorized)
    ));
    let permit = authority
        .admit_join(b.identity.endpoint_id(), request(&team))
        .await
        .unwrap();
    let state = permit.state().clone();
    drop(permit);
    let store = b.create();
    let bad = PublicBootstrap {
        expected_team: team.clone(),
        expected_founder: Some(b.identity.endpoint_id()),
        authenticated_inviter: a.identity.endpoint_id(),
        state: state.clone(),
    };
    assert!(matches!(
        AdmittedTeam::bootstrap(
            store,
            ownership(&b),
            b.identity.endpoint_id(),
            || Ok(NOW + 100),
            None,
            bad
        )
        .await,
        Err(PeerError::Invalid)
    ));
    assert!(matches!(
        b.store().load_status(NOW + 100).unwrap(),
        full_team_store::LoadedStatus::Empty(_)
    ));
    let good = PublicBootstrap {
        expected_team: team.clone(),
        expected_founder: Some(a.identity.endpoint_id()),
        authenticated_inviter: a.identity.endpoint_id(),
        state: state.clone(),
    };
    let joined = AdmittedTeam::bootstrap(
        b.store(),
        ownership(&b),
        b.identity.endpoint_id(),
        || Ok(NOW + 100),
        None,
        good,
    )
    .await
    .unwrap();
    drop(joined);
    let repeat = PublicBootstrap {
        expected_team: team,
        expected_founder: None,
        authenticated_inviter: a.identity.endpoint_id(),
        state,
    };
    assert!(matches!(
        AdmittedTeam::bootstrap(
            b.store(),
            ownership(&b),
            b.identity.endpoint_id(),
            || Ok(NOW + 100),
            None,
            repeat
        )
        .await,
        Err(PeerError::Invalid)
    ));
}
#[tokio::test]
async fn cancelled_bootstrap_retains_real_ownership_until_detached_commit_finishes() {
    let (a, b) = (Node::new(), Node::new());
    let team = invitation(&a);
    let authority = a.authority(false);
    let permit = authority
        .admit_join(b.identity.endpoint_id(), request(&team))
        .await
        .unwrap();
    let input = PublicBootstrap {
        expected_team: team,
        expected_founder: Some(a.identity.endpoint_id()),
        authenticated_inviter: a.identity.endpoint_id(),
        state: permit.state().clone(),
    };
    drop(permit);
    let store = b.create();
    let ownership = ownership(&b);
    let local = b.identity.endpoint_id();
    let sql = rusqlite::Connection::open(b.root.join("full-team.sqlite3")).unwrap();
    sql.execute_batch("BEGIN IMMEDIATE").unwrap();
    let worker = tokio::spawn(AdmittedTeam::bootstrap(
        store,
        ownership,
        local,
        || Ok(NOW + 100),
        None,
        input,
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    worker.abort();
    assert!(worker.await.err().unwrap().is_cancelled());
    let directory = Dir::open_ambient_dir(&b.root, ambient_authority()).unwrap();
    assert!(matches!(
        daemon_lease::DaemonLease::acquire(&directory),
        Err(daemon_lease::LeaseError::AlreadyRunning)
    ));
    sql.execute_batch("ROLLBACK").unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            match daemon_lease::DaemonLease::acquire(&directory) {
                Ok(lease) => {
                    drop(lease);
                    break;
                }
                Err(daemon_lease::LeaseError::AlreadyRunning) => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(error) => panic!("unexpected lease error: {error}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(
        b.store()
            .load(NOW + 100)
            .unwrap()
            .document()
            .unwrap()
            .members[&local.to_string()]
            .body
            .active
    );
}
