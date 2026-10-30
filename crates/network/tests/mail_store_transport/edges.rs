use super::*;
#[test]
fn complete_escaped_wire_envelope_survives_queue_mailbox_and_peek_ack() {
    let root = Root::new();
    let mut store = root.open();
    let peer = peer(11);
    let mut wire = message("escaped", &"\0".repeat(64 * 1024), 1);
    wire.from_agent = None;
    wire.from_repo = None;
    let bytes = serde_json::to_vec(&wire).unwrap();
    assert!(bytes.len() > 256 * 1024);
    let queued = store.enqueue_transport("team", peer, &wire).unwrap();
    assert_eq!(
        store
            .transport_message("escaped")
            .unwrap()
            .unwrap()
            .revision,
        queued.revision
    );
    let incoming = Message {
        id: wire.id.clone(),
        sender: wire.from.clone(),
        recipient: "local-key".into(),
        body: bytes.clone(),
        encoding: PayloadEncoding::WireJson,
        reply_to: wire.reply_to.clone(),
        intent: DeliveryIntent {
            wake: wire.wake,
            notify: wire.notify,
        },
    };
    store.insert(&incoming, State::Pending).unwrap();
    let read = store.poll_batch("local-key", "session", 2, true).unwrap();
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].message.body, bytes);
    assert_eq!(
        read[0].message.display_body().unwrap(),
        wire.body.as_bytes()
    );
    store
        .acknowledge(read[0].attempt.as_ref().unwrap(), "session", 3)
        .unwrap();
    assert_eq!(store.get("escaped").unwrap().state, State::Delivered);
    assert_eq!(store.transport_for("team", peer, 3).unwrap().len(), 1);
}
#[test]
fn concurrent_global_replacement_and_old_receipt_never_remove_new_payload() {
    use std::sync::{Arc, Barrier};
    let root = Root::new();
    let mut store = root.open();
    let one = peer(12);
    let two = peer(13);
    let queued = store
        .enqueue_transport("team", one, &message("same", "old", 0))
        .unwrap();
    let old = store.begin_transport_attempt(&queued, 1).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let before = barrier.clone();
    let path = root.0.clone();
    let receipt = std::thread::spawn(move || {
        let mut store = MailStore::open(&path).unwrap();
        before.wait();
        store.finish_transport_attempt(&old, one, 1, TransportDisposition::Queued)
    });
    let path = root.0.clone();
    let replacement = std::thread::spawn(move || {
        let mut store = MailStore::open(&path).unwrap();
        barrier.wait();
        store
            .enqueue_transport("other-team", two, &message("same", "new", 1))
            .unwrap()
    });
    let result = receipt.join().unwrap();
    assert!(result.is_ok() || matches!(result, Err(MailError::StaleAttempt)));
    let new = replacement.join().unwrap();
    let current = store.transport_message("same").unwrap().unwrap();
    assert_eq!(current.peer, two);
    assert_eq!(current.team, "other-team");
    assert_eq!(current.message.body, "new");
    assert_eq!(current.revision, new.revision);
}
