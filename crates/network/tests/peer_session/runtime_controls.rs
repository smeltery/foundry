use super::*;

#[tokio::test]
async fn empty_owner_forms_publishes_and_reopens_without_reset() {
    let a = Node::new();
    let authority = AdmittedTeam::open(
        a.create(),
        ownership(&a),
        a.identity.endpoint_id(),
        || Ok(NOW + 100),
        None,
    )
    .unwrap();
    assert!(matches!(
        authority.status().await.unwrap(),
        AuthorityStatus::Unjoined
    ));
    let formed = authority
        .form("runtime fixture".into(), "alice".into(), "desktop".into())
        .await
        .unwrap();
    let id = formed.state().id.clone();
    assert_eq!(formed.profile().team_alpn, profile().team_alpn);
    drop(formed);
    assert!(
        authority
            .form("reset".into(), "alice".into(), "desktop".into())
            .await
            .is_err()
    );
    let publication = authority.rotate_link().await.unwrap();
    let link = publication
        .state()
        .link
        .as_ref()
        .unwrap()
        .body
        .secret
        .clone();
    drop(publication);
    let publication = authority
        .issue_invite("another-access".into(), 1, true)
        .await
        .unwrap();
    assert!(publication.state().link.as_ref().unwrap().body.secret == link);
    drop(publication);
    drop(authority);
    let authority = AdmittedTeam::open(
        a.store(),
        ownership(&a),
        a.identity.endpoint_id(),
        || Ok(NOW + 200),
        None,
    )
    .unwrap();
    assert!(
        matches!(authority.status().await.unwrap(),AuthorityStatus::Active {team,..} if team==id)
    );
    assert!(
        authority
            .form("reset".into(), "alice".into(), "desktop".into())
            .await
            .is_err()
    );
    authority.depart().await.unwrap();
    assert!(matches!(
        authority.status().await,
        Err(PeerError::Quiescent)
    ));
    drop(authority);
    assert!(matches!(
        AdmittedTeam::open(
            a.store(),
            ownership(&a),
            a.identity.endpoint_id(),
            || Ok(NOW + 300),
            None
        ),
        Err(PeerError::Quiescent)
    ));
}

#[tokio::test]
async fn current_routing_projection_serializes_with_removal_and_privileged_output() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let authority = a.authority(false);
    let view = authority.routing_view().await.unwrap();
    assert_eq!(view.local, a.identity.endpoint_id());
    assert_eq!(view.founder, a.identity.endpoint_id());
    assert!(
        view.members
            .iter()
            .any(|m| m.endpoint == b.identity.endpoint_id() && m.active && m.person == "bob")
    );
    let changing = authority.clone();
    let target = b.identity.endpoint_id();
    let removal = tokio::spawn(async move { changing.remove(vec![target]).await });
    tokio::task::yield_now().await;
    assert!(!removal.is_finished());
    drop(view);
    removal.await.unwrap().unwrap();
    let view = authority.routing_view().await.unwrap();
    assert!(
        view.members
            .iter()
            .any(|m| m.endpoint == target && !m.active)
    );
    drop(view);
    let publication = authority.team_publication().await.unwrap();
    let changing = authority.clone();
    let rotation = tokio::spawn(async move {
        drop(changing.rotate_link().await.unwrap());
    });
    tokio::task::yield_now().await;
    assert!(!rotation.is_finished());
    drop(publication);
    rotation.await.unwrap();
}

#[tokio::test]
async fn empty_owner_forms_with_relay_then_changes_metadata_under_same_gate() {
    let a = Node::new();
    let authority = AdmittedTeam::open(
        a.create(),
        ownership(&a),
        a.identity.endpoint_id(),
        || Ok(NOW + 100),
        None,
    )
    .unwrap();
    let formed = authority
        .form_with_relay(
            "relay fixture".into(),
            "alice".into(),
            "desktop".into(),
            Some("https://127.0.0.1:8443".into()),
        )
        .await
        .unwrap();
    assert_eq!(formed.state().meta.body.name, "relay fixture");
    assert_eq!(
        formed.state().meta.body.relay.as_deref(),
        Some("https://127.0.0.1:8443")
    );
    let initial = formed.state().meta.body.ts;
    drop(formed);
    let changed = authority.set_relay(None).await.unwrap();
    assert_eq!(changed.state().meta.body.name, "relay fixture");
    assert!(changed.state().meta.body.relay.is_none());
    assert!(changed.state().meta.body.ts > initial);
}

#[tokio::test]
async fn explicit_empty_formation_keeps_code_policy_for_actual_issuance() {
    let a = Node::new();
    let policy = full_team_store::StorePolicy::new(profile(), None)
        .unwrap()
        .with_codes(super::codes::policy())
        .unwrap();
    let store =
        full_team_store::FullTeamStore::create_new(&a.root, a.identity.clone(), policy, WAIT)
            .unwrap();
    let authority = AdmittedTeam::open(
        store,
        ownership(&a),
        a.identity.endpoint_id(),
        || Ok(NOW + 100),
        None,
    )
    .unwrap();
    assert!(authority.issue_code(1, false).await.is_err());
    drop(
        authority
            .form("code fixture".into(), "alice".into(), "desktop".into())
            .await
            .unwrap(),
    );
    let issued = authority.issue_code(1, false).await.unwrap();
    assert!(issued.issued().listener_intent().is_some());
    assert!(!issued.issued().code().is_empty());
}
