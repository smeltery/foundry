use super::*;
use foundry_network::join_protocol::{JoinResult, Refusal, join_peer, serve_join};

#[tokio::test]
async fn one_accept_loop_routes_real_join_then_team_without_second_accept() {
    let (a, b) = (Node::new(), Node::new());
    form(&a, "alice");
    let authority = a.authority(false);
    let publication = authority
        .issue_invite("runtime-access".into(), 1, false)
        .await
        .unwrap();
    let team = publication.state().id.clone();
    drop(publication);
    let unjoined = AdmittedTeam::open(
        b.create(),
        ownership(&b),
        b.identity.endpoint_id(),
        || Ok(NOW + 100),
        None,
    )
    .unwrap();
    assert!(matches!(
        unjoined.status().await.unwrap(),
        AuthorityStatus::Unjoined
    ));
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let router = async {
        let accepted = left.accept_routed(WAIT).await.unwrap();
        assert!(matches!(accepted.channel(), Channel::Join));
        assert_eq!(accepted.remote_id(), right.id());
        let connection = left.into_connection(accepted).unwrap();
        let approved = serve_join(connection, &profile(), WAIT, |peer, request| {
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
        assert!(approved);
        let accepted = left.accept_routed(WAIT).await.unwrap();
        assert!(matches!(accepted.channel(), Channel::Team));
        PeerSession::accept_connection(&left, accepted, authority.clone(), options(&cancel))
            .await
            .unwrap()
    };
    let joining = async {
        let request = team::JoinRequest {
            team: team.clone(),
            secret: "runtime-access".into(),
            person: "bob".into(),
            machine: "desktop".into(),
            proof: None,
            owner_only: false,
        };
        let joined = match join_peer(&right, left.address(), &request, WAIT)
            .await
            .unwrap()
        {
            JoinResult::Joined(joined) => joined,
            _ => panic!("JOIN refused"),
        };
        assert_eq!(joined.peer, left.id());
        unjoined
            .join_existing(PublicBootstrap {
                expected_team: team.clone(),
                expected_founder: None,
                authenticated_inviter: joined.peer,
                state: joined.state,
            })
            .await
            .unwrap();
        assert!(matches!(
            unjoined.status().await.unwrap(),
            AuthorityStatus::Active { .. }
        ));
        PeerSession::connect(&right, left.address(), unjoined.clone(), options(&cancel))
            .await
            .unwrap()
    };
    let (mut server, mut client) = tokio::join!(router, joining);
    client
        .publish_presence(wire::Presence::default())
        .await
        .unwrap();
    assert!(
        matches!(server.receive().await.unwrap(),PeerEvent::Presence {peer,..} if peer==right.id())
    );
    drop((client, server));
    tokio::join!(left.shutdown(), right.shutdown());
}

#[tokio::test]
async fn accepted_origin_rejects_another_instance_even_with_identical_identity() {
    let (a, b) = (Node::new(), Node::new());
    let (origin, other, remote) = (a.endpoint().await, a.endpoint().await, b.endpoint().await);
    assert_eq!(origin.id(), other.id());
    let (connected, accepted) = tokio::join!(
        remote.connect(origin.address(), Channel::Team, WAIT),
        origin.accept_routed(WAIT)
    );
    let connected = connected.unwrap();
    assert!(other.into_connection(accepted.unwrap()).is_err());
    tokio::time::timeout(WAIT, connected.closed())
        .await
        .unwrap();
    tokio::join!(origin.shutdown(), other.shutdown(), remote.shutdown());
}

#[tokio::test]
async fn already_accepted_join_cannot_be_misrouted_as_team() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (connected, accepted) = tokio::join!(
        left.connect(right.address(), Channel::Join, WAIT),
        right.accept_routed(WAIT)
    );
    let connected = connected.unwrap();
    let result = PeerSession::accept_connection(
        &right,
        accepted.unwrap(),
        b.authority(false),
        options(&cancel),
    )
    .await;
    assert!(matches!(result, Err(PeerError::Protocol)));
    tokio::time::timeout(WAIT, connected.closed())
        .await
        .unwrap();
    tokio::join!(left.shutdown(), right.shutdown());
}
