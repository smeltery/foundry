use super::{fixture::*, *};
use crate::daemon_network::{dispatch, driver, events, outbox};
use crate::{mail_store::MailStore, peer_session::PeerEvent, wire};
use std::time::{Duration, Instant};

fn message() -> wire::Message {
    wire::Message {
        id: "bounded-receipt".into(),
        from: "amber/desk/terminal".into(),
        to: "birch/desk/terminal".into(),
        body: "immutable queue regression".into(),
        sent_at: crate::daemon_network::now().unwrap(),
        wake: false,
        notify: false,
        from_agent: None,
        from_repo: None,
        via: None,
        reply_to: None,
        approval: None,
        files: Vec::new(),
    }
}

#[tokio::test]
async fn actual_driver_pump_keeps_a_receipt_out_of_a_presence_backlog() {
    let (a, b) = (Node::open("amber").await, Node::open("birch").await);
    admit(&a, &b).await;
    let (cancel, _) = watch::channel(false);
    let (session, mut remote) = pair(&a, &b, &cancel).await;
    let outbox = outbox::Outbox::open(&a.state).await.unwrap();
    let (send, mut receive) = mpsc::channel(EVENT_BUDGET);
    let peer = b.state.endpoint.id();
    let reservation = a
        .state
        .connections
        .outgoing(a.state.endpoint.id(), peer)
        .unwrap();
    let connection = driver::install(
        &a.state,
        &outbox,
        crate::daemon_network::connections::Ready {
            session,
            reservation,
        },
        1,
        true,
        send,
    )
    .await
    .unwrap();
    let mut live = BTreeMap::from([(peer, connection)]);
    let view = a.state.authority.routing_view().await.unwrap();
    MailStore::open(&a.root)
        .unwrap()
        .enqueue_transport(&view.team, peer, &message())
        .unwrap();
    drop(view);
    let started = Instant::now();
    dispatch::batch(&a.state, &outbox, &mut live).await.unwrap();
    // The actor is deliberately not consuming events, while actual streams and
    // the driver/pump continue. This burst fits both original bounded lanes.
    for n in 0..16 {
        remote
            .publish_presence(Presence {
                features: vec![n.to_string()],
                ..Default::default()
            })
            .await
            .unwrap();
    }
    loop {
        if matches!(
            remote.receive().await.unwrap(),
            PeerEvent::Received {
                duplicate: false,
                ..
            }
        ) {
            break;
        }
    }
    let first = tokio::time::timeout(Duration::from_secs(10), receive.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            &first.event,
            Some(DriverEvent::Peer(PeerEvent::Receipt { .. }))
        ),
        "actual receipt was queued behind replaceable presence"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "receipt did not reach the actor boundary before its deadline"
    );
    for event in ready(&mut receive, Some(first)) {
        events::handle(&a.state, &outbox, &mut live, event)
            .await
            .unwrap();
    }
    assert!(
        MailStore::open(&a.root)
            .unwrap()
            .transport_message("bounded-receipt")
            .unwrap()
            .is_none()
    );
    let mut cursor = None;
    let projection = presence(&mut live, &mut cursor).unwrap();
    match &projection.event {
        Some(DriverEvent::Peer(PeerEvent::Presence { presence, .. })) => {
            assert_eq!(presence.features, ["15"])
        }
        _ => panic!("latest actual presence required"),
    }
    events::handle(&a.state, &outbox, &mut live, projection)
        .await
        .unwrap();
    assert!(presence(&mut live, &mut cursor).is_none());
    assert_eq!(
        MailStore::open(&b.root)
            .unwrap()
            .unread(&crate::daemon_engine::mailbox_address(
                &crate::daemon_engine::Conversation {
                    person: "birch".into(),
                    machine: "desk".into(),
                    name: "terminal".into()
                }
            ))
            .unwrap(),
        1
    );
    driver::retire(&a.state, &outbox, live.remove(&peer).unwrap(), peer)
        .await
        .unwrap();
    drop(remote);
    drop(cancel);
    drop(outbox);
    drop(receive);
    drop(live);
    a.close().await;
    b.close().await;
}

#[tokio::test]
async fn presence_projection_rotates_and_cannot_erase_a_newer_version_or_cross_retirement() {
    let (a, b, c) = (
        Node::open("amber").await,
        Node::open("birch").await,
        Node::open("cedar").await,
    );
    admit(&a, &b).await;
    admit(&a, &c).await;
    let (cancel, _) = watch::channel(false);
    let (ab, mut rb) = pair(&a, &b, &cancel).await;
    let (ac, mut rc) = pair(&a, &c, &cancel).await;
    let outbox = outbox::Outbox::open(&a.state).await.unwrap();
    let (send, receive) = mpsc::channel(EVENT_BUDGET);
    let mut live = BTreeMap::new();
    for (peer, session) in [(b.state.endpoint.id(), ab), (c.state.endpoint.id(), ac)] {
        let reservation = a
            .state
            .connections
            .outgoing(a.state.endpoint.id(), peer)
            .unwrap();
        live.insert(
            peer,
            driver::install(
                &a.state,
                &outbox,
                crate::daemon_network::connections::Ready {
                    session,
                    reservation,
                },
                1,
                true,
                send.clone(),
            )
            .await
            .unwrap(),
        );
    }
    let initial = Presence {
        features: vec!["initial".into()],
        ..Default::default()
    };
    rb.publish_presence(initial.clone()).await.unwrap();
    rc.publish_presence(initial).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !live
            .values()
            .all(|value| value.received_presence.has_changed().unwrap_or(false))
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut cursor = None;
    let first = presence(&mut live, &mut cursor).unwrap();
    let first_peer = first.peer;
    let newer = Presence {
        features: vec!["newer".into()],
        ..Default::default()
    };
    if first_peer == b.state.endpoint.id() {
        rb.publish_presence(newer).await.unwrap();
    } else {
        rc.publish_presence(newer).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        while !live[&first_peer]
            .received_presence
            .has_changed()
            .unwrap_or(false)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Applying an older projection after a new arrival must not mark that new
    // version consumed. The other ready peer still receives the next turn.
    events::handle(&a.state, &outbox, &mut live, first)
        .await
        .unwrap();
    let second = presence(&mut live, &mut cursor).unwrap();
    assert_ne!(second.peer, first_peer);
    events::handle(&a.state, &outbox, &mut live, second)
        .await
        .unwrap();
    let third = presence(&mut live, &mut cursor).unwrap();
    assert_eq!(third.peer, first_peer);
    assert!(
        matches!(&third.event,Some(DriverEvent::Peer(PeerEvent::Presence{presence,..})) if presence.features==["newer"])
    );
    // Invalid attributed peer remains a FIFO close event, not a cached snapshot.
    events::handle(
        &a.state,
        &outbox,
        &mut live,
        Event {
            peer: first_peer,
            generation: 1,
            event: Some(DriverEvent::Peer(PeerEvent::Presence {
                peer: a.state.endpoint.id(),
                presence: Presence::default(),
            })),
        },
    )
    .await
    .unwrap();
    assert!(!live.contains_key(&first_peer));
    events::handle(&a.state, &outbox, &mut live, third)
        .await
        .unwrap();
    assert!(presence(&mut live, &mut cursor).is_none());
    for (peer, connection) in live {
        driver::retire(&a.state, &outbox, connection, peer)
            .await
            .unwrap();
    }
    drop(rb);
    drop(rc);
    drop(cancel);
    drop(outbox);
    drop(send);
    drop(receive);
    a.close().await;
    b.close().await;
    c.close().await;
}
