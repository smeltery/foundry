use super::*;
use crate::peer_session::ReceiptDisposition;

fn peer(seed: u8) -> EndpointId {
    iroh::SecretKey::from_bytes(&[seed; 32]).public()
}
fn receipt(generation: u64) -> Event {
    Event {
        peer: peer(1),
        generation,
        event: Some(DriverEvent::Peer(PeerEvent::Receipt {
            message_id: "queued-message".into(),
            disposition: ReceiptDisposition::Queued,
            note: String::new(),
        })),
    }
}
#[test]
fn snapshots_are_finite_and_keep_receipt_eof_order() {
    let (send, mut receive) = mpsc::channel(EVENT_BUDGET + 1);
    send.try_send(receipt(1))
        .unwrap_or_else(|_| panic!("queue"));
    send.try_send(Event {
        peer: peer(1),
        generation: 1,
        event: None,
    })
    .unwrap_or_else(|_| panic!("queue"));
    for _ in 2..=EVENT_BUDGET {
        send.try_send(receipt(2))
            .unwrap_or_else(|_| panic!("queue"));
    }
    let events = ready(&mut receive, None);
    assert_eq!(events.len(), EVENT_BUDGET);
    assert!(matches!(
        &events[0].event,
        Some(DriverEvent::Peer(PeerEvent::Receipt { .. }))
    ));
    assert!(events[1].event.is_none());
    assert_eq!(receive.len(), 1);
    let selected = receive.try_recv().unwrap();
    assert_eq!(ready(&mut receive, Some(selected)).len(), 1);
}
#[test]
fn presence_is_latest_only_but_invalid_attribution_stays_a_close_event() {
    let (send, mut latest) = watch::channel(None);
    for n in 0..100 {
        let presence = Presence {
            features: vec![n.to_string()],
            ..Default::default()
        };
        assert!(
            coalesce(
                peer(1),
                Some(DriverEvent::Peer(PeerEvent::Presence {
                    peer: peer(1),
                    presence
                })),
                &send
            )
            .is_err()
        );
    }
    assert_eq!(
        latest.borrow_and_update().as_ref().unwrap().features,
        ["99"]
    );
    assert!(!latest.has_changed().unwrap());
    assert!(
        matches!(coalesce(peer(1),Some(DriverEvent::Peer(PeerEvent::Presence{peer:peer(2),presence:Presence::default()})),&send),Ok(Some(DriverEvent::Peer(PeerEvent::Presence{peer:actual,..}))) if actual==peer(2))
    );
}

#[path = "bounded_queue_fixture.rs"]
mod fixture;

#[path = "bounded_queue_runtime_tests.rs"]
mod runtime;

#[path = "bounded_queue_deadline_tests.rs"]
mod deadlines;

#[path = "bounded_queue_lifecycle_tests.rs"]
mod lifecycle;
