use super::*;

#[tokio::test]
async fn presence_and_state_events_follow_actual_authenticated_and_committed_state() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (mut sender, mut receiver) = pair(
        &left,
        &right,
        a.authority(false),
        b.authority(false),
        &cancel,
    )
    .await;
    sender
        .publish_presence(wire::Presence {
            known: vec!["alice/desktop/terminal".into()],
            ..Default::default()
        })
        .await
        .unwrap();
    match receiver.receive().await.unwrap() {
        PeerEvent::Presence { peer, presence } => {
            assert_eq!(peer, left.id());
            assert_eq!(presence.known, ["alice/desktop/terminal"]);
        }
        _ => panic!("expected presence"),
    }
    // External store writers are observed at the next frame boundary. This write
    // occurs between frames; a real supervisor uses the shared writer gate.
    let mut store = a.store();
    let base = store.load(NOW).unwrap();
    let pending = store.prepare_rotation(&base, NOW + 2).unwrap();
    store.commit_transition(pending, NOW + 2).unwrap();
    sender.publish_state().await.unwrap();
    match receiver.receive().await.unwrap() {
        PeerEvent::StateCommitted { revision } => {
            assert_eq!(revision, b.store().load(NOW + 100).unwrap().revision())
        }
        _ => panic!("expected committed state"),
    }
    assert!(
        a.store().load(NOW + 100).unwrap().document()
            == b.store().load(NOW + 100).unwrap().document()
    );
    drop((sender, receiver, store));
    tokio::join!(left.shutdown(), right.shutdown());
}
#[tokio::test]
async fn reload_of_external_removal_rejects_already_connected_peer_frame() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (mut sender, mut receiver) = pair(
        &left,
        &right,
        a.authority(false),
        b.authority(false),
        &cancel,
    )
    .await;
    let mut store = b.store();
    let base = store.load(NOW).unwrap();
    let pending = store
        .prepare_remove(&base, &[a.identity.endpoint_id()], NOW + 1)
        .unwrap();
    store.commit_transition(pending, NOW + 1).unwrap();
    sender
        .publish_presence(wire::Presence::default())
        .await
        .unwrap();
    assert!(matches!(
        receiver.receive().await,
        Err(PeerError::Unauthorized)
    ));
    drop((sender, receiver, store));
    tokio::join!(left.shutdown(), right.shutdown());
}
#[tokio::test]
async fn received_self_removal_commits_then_quiesces_before_any_reply() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let authority = b.authority(false);
    let (mut raw, mut receiver) = raw_pair(&a, &left, &right, authority.clone(), &cancel).await;
    let mut store = a.store();
    let base = store.load(NOW).unwrap();
    let pending = store
        .prepare_remove(&base, &[b.identity.endpoint_id()], NOW + 1)
        .unwrap();
    store.commit_transition(pending, NOW + 1).unwrap();
    let document = store.load(NOW + 100).unwrap().document().unwrap().clone();
    let state = serde_json::from_value(serde_json::to_value(document).unwrap()).unwrap();
    write_frame(
        &mut raw.send,
        &PeerFrame::State {
            state: Box::new(state),
        },
        WAIT,
    )
    .await
    .unwrap();
    assert!(matches!(
        receiver.receive().await,
        Err(PeerError::Quiescent)
    ));
    assert!(matches!(
        b.store().load_status(NOW + 100).unwrap(),
        full_team_store::LoadedStatus::Removed(_)
    ));
    assert_eq!(authority.refresh().await, Err(PeerError::Quiescent));
    assert!(matches!(
        read_frame(&mut raw.receive, WAIT).await,
        Ok(None) | Err(_)
    ));
    drop((raw, receiver, authority, store));
    tokio::join!(left.shutdown(), right.shutdown());
}
#[tokio::test]
async fn cancellation_closes_idle_receiver_and_fences_reuse() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (sender, mut receiver) = pair(
        &left,
        &right,
        a.authority(false),
        b.authority(false),
        &cancel,
    )
    .await;
    cancel.send(true).unwrap();
    assert!(matches!(receiver.receive().await, Err(PeerError::Closed)));
    assert_eq!(receiver.publish_state().await, Err(PeerError::Closed));
    drop((sender, receiver));
    tokio::join!(left.shutdown(), right.shutdown());
}
#[tokio::test]
async fn slow_or_idle_reader_deadline_is_explicit_and_partial_operation_not_reused() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (a_opts, b_opts) = (options(&cancel), options(&cancel));
    let (sender, receiver) = tokio::join!(
        PeerSession::connect(&left, right.address(), a.authority(false), a_opts),
        PeerSession::accept(&right, b.authority(false), b_opts)
    );
    let (sender, mut receiver) = (sender.unwrap(), receiver.unwrap());
    assert!(matches!(receiver.receive().await, Err(PeerError::TimedOut)));
    assert_eq!(receiver.publish_state().await, Err(PeerError::Closed));
    drop((sender, receiver));
    tokio::join!(left.shutdown(), right.shutdown());
}

#[tokio::test]
async fn cancelled_blocking_removal_retains_actual_gate_lease_until_commit_finishes() {
    use cap_std::{ambient_authority, fs::Dir};
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let authority = a.authority(false);
    let sql = rusqlite::Connection::open(a.root.join("full-team.sqlite3")).unwrap();
    sql.execute_batch("BEGIN IMMEDIATE").unwrap();
    let endpoint = b.identity.endpoint_id();
    let worker = tokio::spawn(async move { authority.remove(vec![endpoint]).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    let directory = Dir::open_ambient_dir(&a.root, ambient_authority()).unwrap();
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
    let document = a
        .store()
        .load(NOW + 100)
        .unwrap()
        .document()
        .unwrap()
        .clone();
    assert!(!document.members[&endpoint.to_string()].body.active);
}

#[tokio::test]
async fn real_peer_that_stops_reading_cannot_hold_writer_forever() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (raw, mut server) = raw_pair(&a, &left, &right, b.authority(false), &cancel).await;
    // No further application reads. Existing QUIC credit cannot be recalled, but
    // both flow-control credit and sender buffering are finite.
    raw.connection.set_receive_window(0u32.into());
    let presence = wire::Presence {
        sessions: (0..MAX_PRESENCE)
            .map(|index| wire::SessionPresence {
                address: format!("bob/desktop/session-{index}"),
                name: format!("session-{index}"),
                person: "bob".into(),
                machine: "desktop".into(),
                agent: "x".repeat(512),
                cwd: "x".repeat(512),
                repo: Some("x".repeat(512)),
                branch: Some("x".repeat(512)),
                status: "x".repeat(512),
                since: None,
                doing: Some("x".repeat(512)),
                tier: wire::DeliveryTier::Pull,
                unread: 0,
                is_me: false,
            })
            .collect(),
        ..Default::default()
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        for _ in 0..64 {
            match server.publish_presence(presence.clone()).await {
                Ok(()) => {}
                Err(PeerError::TimedOut) => return,
                Err(error) => panic!("unexpected backpressure result: {error}"),
            }
        }
        panic!("expected bounded writer backpressure");
    })
    .await
    .unwrap();
    assert_eq!(server.publish_state().await, Err(PeerError::Closed));
    drop((raw, server));
    tokio::join!(left.shutdown(), right.shutdown());
}

#[tokio::test]
async fn numeric_version_mismatch_warns_but_commits_actual_state_exchange() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let mut store = a.store();
    let base = store.load(NOW).unwrap();
    let pending = store.prepare_rotation(&base, NOW + 1).unwrap();
    store.commit_transition(pending, NOW + 1).unwrap();
    let expected = store.load(NOW + 100).unwrap().document().unwrap().clone();
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let mut left_options = options(&cancel);
    left_options.profile.protocol = 9;
    let (sender, receiver) = tokio::join!(
        PeerSession::connect(&left, right.address(), a.authority(false), left_options),
        PeerSession::accept(&right, b.authority(false), options(&cancel))
    );
    let (mut sender, mut receiver) = (sender.unwrap(), receiver.unwrap());
    assert_eq!(
        sender.protocol_mismatch(),
        Some(ProtocolMismatch {
            expected: 9,
            observed: 1
        })
    );
    assert_eq!(
        receiver.protocol_mismatch(),
        Some(ProtocolMismatch {
            expected: 1,
            observed: 9
        })
    );
    assert!(b.store().load(NOW + 100).unwrap().document() == Some(&expected));
    sender
        .publish_presence(wire::Presence::default())
        .await
        .unwrap();
    assert!(matches!(
        receiver.receive().await.unwrap(),
        PeerEvent::Presence { .. }
    ));
    drop((sender, receiver, store));
    tokio::join!(left.shutdown(), right.shutdown());
}
