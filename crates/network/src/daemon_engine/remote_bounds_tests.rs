use super::*;

#[test]
fn remote_text_bound_rejects_before_enqueue_and_preserves_later_sends() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(71));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "sender", &root.0);
    let remote = member(8, "blair", "remote");
    engine
        .sync_remote_members("team", 1, std::slice::from_ref(&remote))
        .unwrap();
    engine
        .remote_presence(remote.peer, 1, &presence("blair", "remote", "reader"))
        .unwrap();
    let boundary = "a".repeat(65_536);
    assert!(matches!(
        ok(&mut engine, 1, send(&auth, "boundary", "reader", &boundary)),
        Outcome::TransportQueued { .. }
    ));
    let mut store = MailStore::open(&root.0).unwrap();
    for body in ["b".repeat(65_537), format!("{}x", "é".repeat(32_768))] {
        assert_eq!(body.len(), 65_537);
        assert!(matches!(
            handle(&mut engine, 1, send(&auth, "oversized", "reader", &body)),
            ReplyResult::Error(EngineError::InvalidPayload)
        ));
        let queued = store.transport_for("team", remote.peer, 71).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].message.id, "boundary");
        assert!(queued[0].message.body == boundary);
    }
    assert!(matches!(
        ok(&mut engine, 1, send(&auth, "later", "reader", "legal")),
        Outcome::TransportQueued { .. }
    ));
    let queued = store.transport_for("team", remote.peer, 71).unwrap();
    assert_eq!(queued.len(), 2);
    assert!(
        queued
            .iter()
            .any(|q| q.message.id == "later" && q.message.body == "legal")
    );
    assert!(queued.iter().all(|q| q.message.id != "oversized"));
}

#[test]
fn local_text_keeps_existing_256_kib_limit() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(71));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "sender", &root.0);
    let recipient = register(&mut engine, 2, "local", &root.0);
    let body = "a".repeat(256 * 1024);
    assert!(matches!(
        ok(&mut engine, 1, send(&auth, "local-limit", "local", &body)),
        Outcome::Sent { .. }
    ));
    let received = inbox(&mut engine, 2, &recipient, true).unwrap();
    assert_eq!(received.message_id, "local-limit");
    assert!(
        data_encoding::BASE64
            .decode(received.body_base64.as_bytes())
            .unwrap()
            == body.as_bytes()
    );
}
