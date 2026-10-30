use super::*;

#[tokio::test]
async fn ack_states_keep_exact_open_state_and_note_semantics() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (mut raw, mut server) = raw_pair(&a, &left, &right, b.authority(false), &cancel).await;
    for (n, state) in [
        "queued",
        "delivered",
        "held",
        "refused",
        "retry",
        "notified",
        "not notified",
        "future terminal",
        "",
        "RETRY",
        "not_notified",
    ]
    .iter()
    .enumerate()
    {
        let mut outbound = message();
        std::mem::swap(&mut outbound.from, &mut outbound.to);
        outbound.id = format!("open-ack-{n}");
        let id = outbound.id.clone();
        server.send_text(outbound).await.unwrap();
        assert!(matches!(
            read_frame(&mut raw.receive, WAIT).await.unwrap(),
            Some(PeerFrame::Msg { .. })
        ));
        let note = "raw note\n\0é";
        write_frame(
            &mut raw.send,
            &PeerFrame::Ack {
                id: id.clone(),
                state: (*state).into(),
                note: note.into(),
            },
            WAIT,
        )
        .await
        .unwrap();
        match server.receive().await.unwrap() {
            PeerEvent::Receipt {
                message_id,
                disposition,
                note: actual,
            } => {
                assert_eq!(message_id, id);
                assert_eq!(actual, note);
                assert_eq!(disposition.as_state(), *state);
                assert_eq!(
                    disposition.taken(),
                    *state != "refused" && *state != "retry"
                );
            }
            _ => panic!("raw receipt lost"),
        }
    }
    drop((raw, server));
    tokio::join!(left.shutdown(), right.shutdown());
}

#[tokio::test]
async fn id_replacement_requires_new_connection_before_a_new_ack_owner_exists() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let authority = b.authority(false);
    let (mut raw, mut server) = raw_pair(&a, &left, &right, authority.clone(), &cancel).await;
    let mut outbound = message();
    std::mem::swap(&mut outbound.from, &mut outbound.to);
    server.send_text(outbound.clone()).await.unwrap();
    read_frame(&mut raw.receive, WAIT).await.unwrap().unwrap();
    write_frame(
        &mut raw.send,
        &PeerFrame::Ack {
            id: outbound.id.clone(),
            state: "queued".into(),
            note: String::new(),
        },
        WAIT,
    )
    .await
    .unwrap();
    assert!(matches!(
        server.receive().await.unwrap(),
        PeerEvent::Queued { .. }
    ));
    outbound.body = "replacement body".into();
    assert_eq!(
        server.send_text(outbound.clone()).await,
        Err(PeerError::StaleEpoch)
    );
    assert!(!matches!(
        read_frame(&mut raw.receive, WAIT).await,
        Ok(Some(PeerFrame::Msg { .. }))
    ));
    drop((raw, server));
    let (mut raw, mut fresh) = raw_pair(&a, &left, &right, authority, &cancel).await;
    fresh.send_text(outbound).await.unwrap();
    assert!(
        matches!(read_frame(&mut raw.receive,WAIT).await.unwrap(),Some(PeerFrame::Msg{msg}) if msg.body=="replacement body")
    );
    drop((raw, fresh));
    tokio::join!(left.shutdown(), right.shutdown());
}

#[tokio::test]
async fn durable_receipt_wakes_inbox_even_when_following_ack_output_fails() {
    use tokio::sync::{Notify, oneshot};
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let arrived = Arc::new(Notify::new());
    let notified = arrived.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    let (entered, at_resolver) = oneshot::channel();
    let entered = std::sync::Mutex::new(Some(entered));
    let (resume, release) = std::sync::mpsc::channel();
    let release = std::sync::Mutex::new(release);
    let mailbox = TextMailbox::new(
        mail_store::MailStore::open(&b.root).unwrap(),
        move |address| {
            entered.lock().unwrap().take().unwrap().send(()).unwrap();
            release.lock().unwrap().recv_timeout(WAIT).unwrap();
            Ok(address.into())
        },
    )
    .with_notification(arrived.clone());
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
    let (mut raw, mut receiver) = raw_pair(&a, &left, &right, authority, &cancel).await;
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
    // The complete Msg is already authenticated/decoded in the blocked resolver.
    // Close transport before allowing durable insertion and its attempted ACK.
    raw.connection.close(0u32.into(), b"fixture loses receipt");
    tokio::time::sleep(Duration::from_millis(50)).await;
    resume.send(()).unwrap();
    tokio::time::timeout(WAIT, notified).await.unwrap();
    assert!(receiving.await.unwrap().is_err());
    let store = mail_store::MailStore::open(&b.root).unwrap();
    let stored = store.get("receipt-1").unwrap();
    assert_eq!(stored.state, mail_store::State::Pending);
    assert_eq!(
        stored.message.encoding,
        mail_store::PayloadEncoding::WireJson
    );
    assert_eq!(
        stored.message.display_body().unwrap(),
        message().body.as_bytes()
    );
    drop((store, raw));
    tokio::join!(left.shutdown(), right.shutdown());
}

#[tokio::test]
async fn dedicated_reader_allows_idle_but_times_out_a_started_frame() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let document = a
        .store()
        .load(NOW + 100)
        .unwrap()
        .document()
        .unwrap()
        .clone();
    let dial = async {
        let connection = left
            .connect(right.address(), Channel::Team, WAIT)
            .await
            .unwrap();
        let (mut send, mut receive) = connection.open_bi().await.unwrap();
        // Exercise delayed protocol setup before testing the idle reader.
        tokio::time::sleep(Duration::from_millis(250)).await;
        write_frame(
            &mut send,
            &PeerFrame::Hello {
                team: document.id.clone(),
                proto: 1,
            },
            WAIT,
        )
        .await
        .unwrap();
        read_frame(&mut receive, WAIT).await.unwrap();
        let state = serde_json::from_value(serde_json::to_value(document).unwrap()).unwrap();
        write_frame(
            &mut send,
            &PeerFrame::State {
                state: Box::new(state),
            },
            WAIT,
        )
        .await
        .unwrap();
        read_frame(&mut receive, WAIT).await.unwrap();
        RawPeer {
            connection,
            send,
            receive,
        }
    };
    let opt = options(&cancel);
    let (raw, server) = tokio::join!(dial, PeerSession::accept(&right, b.authority(false), opt));
    let (mut raw, mut server) = (raw, server.unwrap());
    let mut reader = server.take_reader().unwrap();
    let (started, read_started) = tokio::sync::oneshot::channel();
    let reading = tokio::spawn(async move {
        started.send(()).unwrap();
        reader.next().await
    });
    read_started.await.unwrap();
    tokio::time::sleep(WAIT + Duration::from_millis(250)).await;
    assert!(!reading.is_finished());
    raw.send.write_all(&[0]).await.unwrap();
    assert!(matches!(
        tokio::time::timeout(WAIT * 2, reading)
            .await
            .unwrap()
            .unwrap(),
        Err(PeerError::TimedOut)
    ));
    drop((raw, server));
    tokio::join!(left.shutdown(), right.shutdown());
}
