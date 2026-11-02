use super::*;

#[tokio::test]
async fn escape_heavy_legal_text_and_supported_annotations_survive_real_receipt() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (mut sender, mut receiver) = pair(
        &left,
        &right,
        a.authority(false),
        b.authority(true),
        &cancel,
    )
    .await;
    let mut original = message();
    original.body = "\0".repeat(64 * 1024);
    original.from_agent = Some(format!(" !agent-{}é", "x".repeat(40)));
    original.from_repo = Some("/work/a_b;c$-d".into());
    original.via = Some("route:*@host /x?z".into());
    original.reply_to = Some("a-b_😀C!".into());
    assert!(serde_json::to_vec(&original).unwrap().len() > 256 * 1024);
    sender.send_text(original.clone()).await.unwrap();
    assert!(matches!(
        receiver.receive().await.unwrap(),
        PeerEvent::Received {
            duplicate: false,
            ..
        }
    ));
    assert!(matches!(
        sender.receive().await.unwrap(),
        PeerEvent::Queued { .. }
    ));
    let store = mail_store::MailStore::open(&b.root).unwrap();
    let stored = store.get(&original.id).unwrap();
    assert_eq!(
        stored.message.encoding,
        mail_store::PayloadEncoding::WireJson
    );
    assert!(stored.message.body.len() > 256 * 1024);
    assert_eq!(
        stored.message.display_body().unwrap(),
        original.body.as_bytes()
    );
    let saved: wire::Message = serde_json::from_slice(&stored.message.body).unwrap();
    assert_eq!(saved.from_agent, Some(format!("agent-{}", "x".repeat(24))));
    assert_eq!(saved.from_repo.as_deref(), Some("/work/a_bc-d"));
    assert_eq!(saved.via.as_deref(), Some("route:*@host/xz"));
    assert_eq!(saved.reply_to.as_deref(), Some("abC"));
    drop((sender, receiver, store));
    tokio::join!(left.shutdown(), right.shutdown());
}

#[tokio::test]
async fn oversized_body_is_refused_to_authenticated_peer_without_inbox_mutation() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (mut raw, mut receiver) = raw_pair(&a, &left, &right, b.authority(true), &cancel).await;
    let mut invalid = message();
    invalid.body = "x".repeat(64 * 1024 + 1);
    write_frame(
        &mut raw.send,
        &PeerFrame::Msg {
            msg: Box::new(invalid),
        },
        WAIT,
    )
    .await
    .unwrap();
    let note = match receiver.receive().await.unwrap() {
        PeerEvent::Rejected { message_id, note } => {
            assert_eq!(message_id, "receipt-1");
            note
        }
        _ => panic!("expected explicit rejection"),
    };
    assert!(!note.is_empty());
    assert!(
        matches!(read_frame(&mut raw.receive,WAIT).await.unwrap(),Some(PeerFrame::Ack{id,state,note:actual}) if id=="receipt-1"&&state=="refused"&&actual==note)
    );
    let store = mail_store::MailStore::open(&b.root).unwrap();
    assert_eq!(store.unread("bob/desktop/terminal").unwrap(), 0);
    assert!(matches!(
        store.get("receipt-1"),
        Err(mail_store::MailError::Missing)
    ));
    drop((raw, receiver, store));
    tokio::join!(left.shutdown(), right.shutdown());
}

#[tokio::test]
async fn rejected_recipient_cannot_emit_receipt_after_concurrent_peer_removal() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (entered, at_resolver) = tokio::sync::oneshot::channel();
    let entered = std::sync::Mutex::new(Some(entered));
    let (resume, release) = std::sync::mpsc::channel();
    let release = std::sync::Mutex::new(release);
    let mailbox = TextMailbox::new(mail_store::MailStore::open(&b.root).unwrap(), move |_| {
        entered.lock().unwrap().take().unwrap().send(()).unwrap();
        release.lock().unwrap().recv_timeout(WAIT).unwrap();
        Err(PeerError::Unauthorized)
    });
    let authority = AdmittedTeam::new(
        b.store(),
        ownership(&b),
        b.identity.endpoint_id(),
        || Ok(NOW + 100),
        Some(mailbox),
    )
    .unwrap();
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (mut raw, mut receiver) = raw_pair(&a, &left, &right, authority.clone(), &cancel).await;
    write_frame(
        &mut raw.send,
        &PeerFrame::Msg {
            msg: Box::new(message()),
        },
        WAIT,
    )
    .await
    .unwrap();
    let receiving = tokio::spawn(async move { receiver.receive().await });
    tokio::time::timeout(WAIT, at_resolver)
        .await
        .unwrap()
        .unwrap();
    let (started, removing) = tokio::sync::oneshot::channel();
    let changing = authority.clone();
    let target = a.identity.endpoint_id();
    let removed = tokio::spawn(async move {
        started.send(()).unwrap();
        changing.remove(vec![target]).await
    });
    tokio::time::timeout(WAIT, removing).await.unwrap().unwrap();
    resume.send(()).unwrap();
    removed.await.unwrap().unwrap();
    assert!(matches!(
        receiving.await.unwrap(),
        Err(PeerError::Unauthorized)
    ));
    assert!(matches!(
        read_frame(&mut raw.receive, WAIT).await,
        Ok(None) | Err(_)
    ));
    assert_eq!(
        mail_store::MailStore::open(&b.root)
            .unwrap()
            .unread("bob/desktop/terminal")
            .unwrap(),
        0
    );
    drop(raw);
    tokio::join!(left.shutdown(), right.shutdown());
}
