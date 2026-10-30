use super::{fixture::*, *};
use crate::{
    daemon_network::{dispatch, driver, events, outbox},
    mail_store::MailStore,
    peer_session::{PeerError, PeerEvent, PeerSession},
    wire,
};
use std::time::Duration;

struct Link {
    a: Node,
    b: Node,
    outbox: outbox::Outbox,
    live: BTreeMap<EndpointId, Live>,
    remote: PeerSession,
    receive: mpsc::Receiver<Event>,
    _cancel: watch::Sender<bool>,
}
async fn link(capacity: usize) -> Link {
    let (a, b) = (Node::open("amber").await, Node::open("birch").await);
    admit(&a, &b).await;
    let (cancel, _) = watch::channel(false);
    let (session, remote) = pair(&a, &b, &cancel).await;
    let outbox = outbox::Outbox::open(&a.state).await.unwrap();
    let (send, receive) = mpsc::channel(capacity);
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
    Link {
        a,
        b,
        outbox,
        live: BTreeMap::from([(peer, connection)]),
        remote,
        receive,
        _cancel: cancel,
    }
}
fn mailbox() -> String {
    crate::daemon_engine::mailbox_address(&crate::daemon_engine::Conversation {
        person: "birch".into(),
        machine: "desk".into(),
        name: "terminal".into(),
    })
}
async fn entries(state: &crate::daemon_network::State) -> Vec<crate::daemon_engine::Entry> {
    state
        .engine
        .with(|engine| {
            use crate::daemon_engine::{
                ConnectionId, Filter, Outcome, ReplyResult, Request, RequestEnvelope,
            };
            match engine
                .handle(
                    ConnectionId(99),
                    RequestEnvelope {
                        id: 1,
                        request: Request::Who {
                            filter: Filter {
                                person: Some("birch".into()),
                                ..Default::default()
                            },
                        },
                    },
                )
                .result
            {
                ReplyResult::Ok(Outcome::Directory { entries }) => Ok(entries),
                _ => panic!("actual Engine directory required"),
            }
        })
        .await
        .unwrap()
}
#[tokio::test]
async fn presence_then_eof_keeps_the_closed_watch_snapshot_only_as_offline_directory() {
    let Link {
        a,
        b,
        outbox,
        mut live,
        mut remote,
        mut receive,
        _cancel,
    } = link(EVENT_BUDGET).await;
    let peer = b.state.endpoint.id();
    let snapshot = b
        .state
        .engine
        .with(|engine| engine.local_presence())
        .await
        .unwrap();
    remote.publish_presence(snapshot).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while live[&peer].received_presence.borrow().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(entries(&a.state).await.is_empty());
    drop(remote);
    let eof = tokio::time::timeout(Duration::from_secs(10), receive.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(eof.event.is_none());
    tokio::time::timeout(Duration::from_secs(10), async {
        while !live[&peer].pump.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(live[&peer].received_presence.has_changed().is_err());
    assert!(live[&peer].received_presence.borrow().is_some());
    let observer = a.state.clone();
    let observing = tokio::spawn(async move {
        for _ in 0..16 {
            assert!(entries(&observer).await.iter().all(|entry| !entry.online));
            tokio::task::yield_now().await;
        }
    });
    events::handle(&a.state, &outbox, &mut live, eof)
        .await
        .unwrap();
    observing.await.unwrap();
    assert!(live.is_empty());
    let final_entries = entries(&a.state).await;
    assert_eq!(final_entries.len(), 1);
    assert_eq!(final_entries[0].conversation.name, "terminal");
    assert!(!final_entries[0].online);
    assert_eq!(final_entries[0].status, "offline");
    drop(outbox);
    drop(live);
    drop(receive);
    drop(_cancel);
    a.close().await;
    b.close().await;
}
fn message() -> wire::Message {
    wire::Message {
        id: "retirement-receipt".into(),
        from: "amber/desk/terminal".into(),
        to: "birch/desk/terminal".into(),
        body: "immutable EOF receipt".into(),
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
async fn send(link: &mut Link) -> Vec<u8> {
    let message = message();
    let expected = serde_json::to_vec(&message).unwrap();
    let view = link.a.state.authority.routing_view().await.unwrap();
    MailStore::open(&link.a.root)
        .unwrap()
        .enqueue_transport(&view.team, link.b.state.endpoint.id(), &message)
        .unwrap();
    drop(view);
    dispatch::batch(&link.a.state, &link.outbox, &mut link.live)
        .await
        .unwrap();
    expected
}
async fn accept(remote: &mut PeerSession) {
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
}
async fn settled(
    a: Node,
    b: Node,
    outbox: outbox::Outbox,
    live: BTreeMap<EndpointId, Live>,
    receive: mpsc::Receiver<Event>,
    cancel: watch::Sender<bool>,
    expected: Vec<u8>,
) {
    assert!(live.is_empty());
    assert!(
        MailStore::open(&a.root)
            .unwrap()
            .transport_message("retirement-receipt")
            .unwrap()
            .is_none()
    );
    let receiver = MailStore::open(&b.root).unwrap();
    assert_eq!(receiver.unread(&mailbox()).unwrap(), 1);
    assert_eq!(
        receiver.get("retirement-receipt").unwrap().message.body,
        expected
    );
    drop(receiver);
    drop(outbox);
    drop(live);
    drop(receive);
    drop(cancel);
    a.close().await;
    b.close().await;
}
#[tokio::test]
async fn accepted_receipt_settles_on_eof_even_without_its_actor_notification() {
    let mut link = link(EVENT_BUDGET).await;
    let expected = send(&mut link).await;
    accept(&mut link.remote).await;
    let receipt = tokio::time::timeout(Duration::from_secs(10), link.receive.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        &receipt.event,
        Some(DriverEvent::Peer(PeerEvent::Receipt { .. }))
    ));
    drop(receipt); // The registered work, not this notification, owns settlement.
    let Link {
        a,
        b,
        outbox,
        mut live,
        remote,
        mut receive,
        _cancel,
    } = link;
    drop(remote);
    let eof = tokio::time::timeout(Duration::from_secs(10), receive.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(eof.event.is_none());
    events::handle(&a.state, &outbox, &mut live, eof)
        .await
        .unwrap();
    settled(a, b, outbox, live, receive, _cancel, expected).await;
}
#[tokio::test]
async fn accepted_receipt_survives_actual_driver_event_overflow_without_notification() {
    // One host slot plus the blocked pump plus the driver's32 slots hold34
    // StateCommitted events. The following ACK validates/publishes, but its
    // ordinary event cannot enter the full driver lane and that driver closes.
    let mut link = link(1).await;
    let expected = send(&mut link).await;
    for _ in 0..34 {
        link.remote.publish_state().await.unwrap();
    }
    accept(&mut link.remote).await;
    assert!(matches!(
        link.remote.receive().await,
        Err(PeerError::Closed | PeerError::Transport)
    ));
    let Link {
        a,
        b,
        outbox,
        mut live,
        remote,
        mut receive,
        _cancel,
    } = link;
    drop(remote);
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), receive.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            !matches!(
                &event.event,
                Some(DriverEvent::Peer(PeerEvent::Receipt { .. }))
            ),
            "the forced overflow unexpectedly delivered the ACK notification"
        );
        let eof = event.event.is_none();
        events::handle(&a.state, &outbox, &mut live, event)
            .await
            .unwrap();
        if eof {
            break;
        }
    }
    settled(a, b, outbox, live, receive, _cancel, expected).await;
}

#[tokio::test]
async fn retirement_closes_transport_before_waiting_for_a_busy_authority_gate() {
    let Link {
        a,
        b,
        outbox,
        mut live,
        mut remote,
        receive,
        _cancel,
    } = link(EVENT_BUDGET).await;
    let peer = b.state.endpoint.id();
    let guard = a.state.authority.team_publication().await.unwrap();
    let connection = live.remove(&peer).unwrap();
    let outbox = std::sync::Arc::new(outbox);
    let closing_outbox = outbox.clone();
    let state = a.state.clone();
    let retirement =
        tokio::spawn(
            async move { driver::retire(&state, &closing_outbox, connection, peer).await },
        );
    assert!(matches!(
        remote.receive().await,
        Err(PeerError::Closed | PeerError::Transport)
    ));
    assert!(
        !retirement.is_finished(),
        "the actual authority gate was not retained"
    );
    drop(guard);
    retirement.await.unwrap().unwrap();
    drop(remote);
    drop(outbox);
    drop(live);
    drop(receive);
    drop(_cancel);
    a.close().await;
    b.close().await;
}
