#![cfg(unix)]
mod common;
#[path = "full_team_store_support/mod.rs"]
mod support;
use foundry_network::{
    Channel, NetworkEndpoint,
    daemon_lease::DaemonLease,
    full_team_store::CommitOutcome,
    join_protocol::{JoinResult, Refusal, join_peer, serve_join},
    team::JoinRequest,
    team_state::Admission,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use support::{Fixture, NOW, formation};
const DEADLINE: Duration = Duration::from_secs(5);

async fn exchange(valid: bool) {
    let fixture = Fixture::new();
    let directory =
        cap_std::fs::Dir::open_ambient_dir(&fixture.base.root, cap_std::ambient_authority())
            .unwrap();
    let ownership = Arc::new(DaemonLease::acquire(&directory).unwrap());
    let mut store = fixture.create();
    let empty = store.load(NOW).unwrap();
    let formation = store.prepare_formation(&empty, formation(), NOW).unwrap();
    let base = store.commit_formation(formation).unwrap();
    let team = base.document().unwrap().id.clone();
    let invite = store
        .prepare_invite(&base, "private-invite", 24, false, NOW)
        .unwrap();
    store.commit_transition(invite, NOW).unwrap();
    let store = Arc::new(Mutex::new(store));
    let mut server_config = common::config();
    server_config.profile = fixture.base.profile.clone();
    server_config.secret_key = fixture.identity.secret_key().clone();
    let server = NetworkEndpoint::bind(server_config).await.unwrap();
    let mut client_config = common::config();
    client_config.profile = fixture.base.profile.clone();
    let client = NetworkEndpoint::bind(client_config).await.unwrap();
    let request = JoinRequest {
        team,
        secret: if valid {
            "private-invite"
        } else {
            "wrong-invite"
        }
        .into(),
        person: "bob".into(),
        machine: "laptop".into(),
        proof: None,
        owner_only: false,
    };
    let admission_store = store.clone();
    let profile = fixture.base.profile.clone();
    let host = async {
        let connection = server.accept(DEADLINE).await.unwrap();
        serve_join(
            connection,
            &profile,
            DEADLINE,
            move |peer, request| async move {
                tokio::task::spawn_blocking(move || {
                    let _ownership = ownership;
                    let mut store = admission_store.lock().unwrap();
                    let base = store.load(NOW).map_err(|_| Refusal::Unavailable)?;
                    if base.document().is_none_or(|d| d.id != request.team) {
                        return Err(Refusal::Denied);
                    }
                    let pending = store
                        .prepare_admission(
                            &base,
                            Admission {
                                endpoint: peer,
                                person: &request.person,
                                machine: &request.machine,
                                secret: &request.secret,
                                proof: request.proof.as_deref(),
                                owner_only: request.owner_only,
                            },
                            NOW,
                        )
                        .map_err(|_| Refusal::Denied)?;
                    match store
                        .commit_transition(pending, NOW)
                        .map_err(|_| Refusal::Unavailable)?
                    {
                        CommitOutcome::Active(loaded) | CommitOutcome::Unchanged(loaded) => {
                            Ok(loaded.document().unwrap().clone())
                        }
                        _ => Err(Refusal::Denied),
                    }
                })
                .await
                .map_err(|_| Refusal::Unavailable)?
            },
        )
        .await
        .unwrap()
    };
    let (outcome, accepted) = tokio::join!(
        join_peer(&client, server.address(), &request, DEADLINE),
        host
    );
    let client_id = client.id().to_string();
    {
        let mut store = store.lock().unwrap();
        let loaded = store.load(NOW).unwrap();
        match outcome.unwrap() {
            JoinResult::Joined(joined) => {
                assert!(valid && accepted);
                assert_eq!(joined.peer, server.id());
                assert!(joined.state.members[&client_id].body.active);
                assert!(loaded.document().unwrap().members[&client_id].body.active);
            }
            JoinResult::Refused(error) => {
                assert!(!valid && !accepted);
                assert!(!error.contains("wrong-invite"));
                assert!(!loaded.document().unwrap().members.contains_key(&client_id));
            }
        }
    }
    tokio::join!(client.shutdown(), server.shutdown());
}

#[tokio::test]
async fn real_join_commits_authenticated_endpoint_before_reply() {
    exchange(true).await;
}
#[tokio::test]
async fn real_refusal_preserves_team_and_does_not_echo_secret() {
    exchange(false).await;
}

#[tokio::test]
async fn nonjoin_channel_is_closed_without_invoking_admission() {
    let server = NetworkEndpoint::bind(common::config()).await.unwrap();
    let client = NetworkEndpoint::bind(common::config()).await.unwrap();
    let (outgoing, incoming) = tokio::join!(
        client.connect(server.address(), Channel::Team, DEADLINE),
        server.accept(DEADLINE)
    );
    let outgoing = outgoing.unwrap();
    let result = serve_join::<_, _, foundry_network::team::TeamDocument>(
        incoming.unwrap(),
        &common::profile(),
        DEADLINE,
        |_, _| async { panic!("wrong channel reached admission") },
    )
    .await;
    assert!(result.is_err());
    tokio::time::timeout(DEADLINE, outgoing.closed())
        .await
        .unwrap();
    tokio::join!(client.shutdown(), server.shutdown());
}
