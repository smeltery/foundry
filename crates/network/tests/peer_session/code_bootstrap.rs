use super::codes::{authority, join_request, policy};
use super::*;
use std::sync::atomic::AtomicI64;

#[tokio::test]
async fn actual_code_join_bootstrap_and_team_exchange_use_authenticated_anchor() {
    use foundry_network::join_protocol::{self, JoinResult, Refusal};
    let (a, existing, newcomer) = (Node::new(), Node::new(), Node::new());
    provision(&a, &existing);
    let team = a.store().load(NOW).unwrap().document().unwrap().id.clone();
    let authority = authority(&a, Arc::new(AtomicI64::new(NOW + 100)));
    let issued = authority.issue_code(1, false).await.unwrap();
    let selected_code = policy().normalize(issued.issued().code()).unwrap();
    let ticket = foundry_network::ticket::TicketCodec::new(&profile(), None)
        .unwrap()
        .decode(issued.issued().ticket())
        .unwrap();
    let secret = ticket.secret;
    drop(issued);
    let lease = authority
        .recover_code_intents()
        .await
        .unwrap()
        .pop()
        .unwrap();
    let material = authority.code_permit(lease.clone()).await.unwrap();
    let code_key = material.derived().endpoint_key().clone();
    let code_profile = material.profile().clone();
    drop(material);
    let code_endpoint = NetworkEndpoint::bind(foundry_network::EndpointConfig {
        profile: code_profile,
        secret_key: code_key,
        relay: foundry_network::RelaySelection::Disabled,
        bind_address: "127.0.0.1:0".parse().unwrap(),
        direct_paths: true,
        relay_root_certificates: Vec::new(),
    })
    .await
    .unwrap();
    let joiner = newcomer.endpoint().await;
    let request = join_request("", secret);
    let server = async {
        let connection = code_endpoint.accept(WAIT).await.unwrap();
        join_protocol::serve_join(connection, &profile(), WAIT, |peer, request| {
            let authority = authority.clone();
            let lease = lease.clone();
            async move {
                authority
                    .admit_code_join(peer, request, lease)
                    .await
                    .map_err(|_| Refusal::Denied)
            }
        })
        .await
        .unwrap()
    };
    let client = join_protocol::join_peer(&joiner, code_endpoint.address(), &request, WAIT);
    let (accepted, reply) = tokio::join!(server, client);
    assert!(accepted);
    let JoinResult::Joined(reply) = reply.unwrap() else {
        panic!("code join refused");
    };
    assert_eq!(reply.peer, code_endpoint.id());
    // The actual derived JOIN identity is intentionally NOT a roster inviter.
    assert!(!reply.state.members.contains_key(&reply.peer.to_string()));
    assert!(
        policy()
            .authenticate_endpoint(&selected_code, a.identity.endpoint_id())
            .is_err()
    );
    assert!(reply.state.members[&joiner.id().to_string()].body.active);
    assert_eq!(reply.state.id, team);
    let store = newcomer.create();
    let directory =
        cap_std::fs::Dir::open_ambient_dir(&newcomer.root, cap_std::ambient_authority()).unwrap();
    let ownership = daemon_lease::DaemonLease::acquire(&directory).unwrap();
    let ordinary = PublicBootstrap {
        expected_team: team.clone(),
        expected_founder: Some(a.identity.endpoint_id()),
        authenticated_inviter: reply.peer,
        state: reply.state.clone(),
    };
    assert!(matches!(
        AdmittedTeam::bootstrap(
            store,
            ownership,
            joiner.id(),
            || Ok(NOW + 100),
            None,
            ordinary
        )
        .await,
        Err(PeerError::Invalid)
    ));
    for (expected_team, expected_founder) in
        [(Some("wrong-team".into()), None), (None, Some(joiner.id()))]
    {
        let anchor = policy()
            .authenticate_endpoint(&selected_code, reply.peer)
            .unwrap();
        let ownership = daemon_lease::DaemonLease::acquire(&directory).unwrap();
        let input = PublicCodeBootstrap {
            anchor,
            expected_team,
            expected_founder,
            state: reply.state.clone(),
        };
        assert!(matches!(
            AdmittedTeam::bootstrap_code(
                newcomer.store(),
                ownership,
                joiner.id(),
                || Ok(NOW + 100),
                None,
                input
            )
            .await,
            Err(PeerError::Invalid)
        ));
        assert!(matches!(
            newcomer.store().load_status(NOW + 100).unwrap(),
            full_team_store::LoadedStatus::Empty(_)
        ));
    }
    let anchor = policy()
        .authenticate_endpoint(&selected_code, reply.peer)
        .unwrap();
    let ownership = daemon_lease::DaemonLease::acquire(&directory).unwrap();
    let input = PublicCodeBootstrap {
        anchor,
        expected_team: None,
        expected_founder: None,
        state: reply.state,
    };
    let joined = AdmittedTeam::bootstrap_code(
        newcomer.store(),
        ownership,
        joiner.id(),
        || Ok(NOW + 100),
        None,
        input,
    )
    .await
    .unwrap();
    assert!(
        newcomer
            .store()
            .load(NOW + 100)
            .unwrap()
            .document()
            .unwrap()
            .members[&joiner.id().to_string()]
            .body
            .active
    );
    assert!(authority.recover_code_intents().await.unwrap().is_empty());
    assert!(matches!(
        authority.code_permit(lease).await,
        Err(PeerError::StaleEpoch)
    ));
    let team_endpoint = a.endpoint().await;
    let (cancel, _) = watch::channel(false);
    let (mut sender, mut receiver) =
        pair(&joiner, &team_endpoint, joined, authority, &cancel).await;
    sender
        .publish_presence(wire::Presence {
            known: vec!["new-peer/desktop/terminal".into()],
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        matches!(receiver.receive().await.unwrap(), PeerEvent::Presence { peer, .. } if peer == joiner.id())
    );
    drop((sender, receiver));
    tokio::join!(
        code_endpoint.shutdown(),
        joiner.shutdown(),
        team_endpoint.shutdown()
    );
}
