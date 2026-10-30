use super::{fixture::*, *};
use crate::daemon_network::{dispatch, driver, outbox};
use crate::{mail_store::MailStore, peer_session::PeerEvent, wire};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

fn message() -> wire::Message {
    wire::Message {
        id: "preparation-receipt".into(),
        from: "amber/desk/terminal".into(),
        to: "birch/desk/terminal".into(),
        body: "guarded receipt scan".into(),
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
async fn receipt_published_during_blocked_preparation_settles_before_expiry_scan() {
    preparation(true, false).await;
}
#[tokio::test]
async fn blocked_preparation_without_a_receipt_still_retires_at_five_seconds() {
    preparation(false, false).await;
}
#[tokio::test]
async fn receipt_registration_cannot_settle_after_its_authority_epoch_changes() {
    preparation(true, true).await;
}
async fn preparation(ack: bool, change_epoch: bool) {
    let (a, b) = (Node::open("amber").await, Node::open("birch").await);
    admit(&a, &b).await;
    let (cancel, _) = watch::channel(false);
    let (session, mut remote) = pair(&a, &b, &cancel).await;
    let outbox = Arc::new(outbox::Outbox::open(&a.state).await.unwrap());
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
    let original = MailStore::open(&a.root)
        .unwrap()
        .enqueue_transport(&view.team, peer, &message())
        .unwrap();
    drop(view);
    let before = Instant::now();
    dispatch::batch(&a.state, &outbox, &mut live).await.unwrap();
    let prepared = tokio::time::Instant::now();
    let (entered, waiting) = oneshot::channel();
    let (resume, paused) = oneshot::channel();
    let state = a.state.clone();
    let scan_outbox = outbox.clone();
    // Test-owned orchestration stops at the real preparation/view boundary.
    // No production hook or authority permit is held across this barrier.
    let scan = tokio::spawn(async move {
        entered.send(()).unwrap();
        paused.await.unwrap();
        let view = state.view().await.unwrap().unwrap();
        scan_outbox.batch(view, vec![(peer, 1)]).await.unwrap()
    });
    waiting.await.unwrap();
    for n in 0..8 {
        remote
            .publish_presence(Presence {
                features: vec![n.to_string()],
                ..Default::default()
            })
            .await
            .unwrap();
    }
    if ack {
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
        let event = tokio::time::timeout(Duration::from_secs(10), receive.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            event.event,
            Some(DriverEvent::Peer(PeerEvent::Receipt {
                disposition: ReceiptDisposition::Queued,
                ..
            }))
        ));
        assert!(
            before.elapsed() < Duration::from_secs(5),
            "actual validated receipt must be published before expiry"
        );
        // Hold/drop actor delivery: correctness must come from the permit-bound
        // accepted work, not from the pre-turn drain or this notification.
        if change_epoch {
            drop(
                a.state
                    .authority
                    .issue_invite("epoch-transition".into(), 1, false)
                    .await
                    .unwrap(),
            );
        }
    }
    // Every attempt began before preparation completed; its unchanged five-second
    // deadline is now elapsed when the actual guarded scan resumes.
    tokio::time::sleep_until(prepared + Duration::from_secs(5)).await;
    resume.send(()).unwrap();
    let batch = scan.await.unwrap();
    let queued = MailStore::open(&a.root)
        .unwrap()
        .transport_message("preparation-receipt")
        .unwrap();
    if ack && !change_epoch {
        assert!(
            batch.retire.is_empty(),
            "accepted receipt was overtaken by expiry"
        );
        assert!(batch.messages.is_empty());
        assert!(queued.is_none(), "actual exact outbox item was not settled");
    } else {
        assert_eq!(batch.retire, [(peer, 1)]);
        assert!(queued.as_ref().is_some_and(
            |row| row.message == original.message && row.revision == original.revision
        ));
        assert!(
            before.elapsed() < Duration::from_secs(12),
            "no-ACK expiry was starved"
        );
    }
    driver::retire(&a.state, &outbox, live.remove(&peer).unwrap(), peer)
        .await
        .unwrap();
    if !ack || change_epoch {
        let row = MailStore::open(&a.root)
            .unwrap()
            .transport_message("preparation-receipt")
            .unwrap()
            .unwrap();
        assert!(row.message == original.message && row.revision == original.revision);
    }
    drop(remote);
    drop(cancel);
    drop(outbox);
    drop(receive);
    drop(live);
    a.close().await;
    b.close().await;
}
