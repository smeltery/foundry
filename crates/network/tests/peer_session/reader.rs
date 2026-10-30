use super::*;

#[tokio::test]
async fn partial_inbound_frame_survives_concurrent_output_on_persistent_connection() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (mut raw, mut server) = raw_pair(&a, &left, &right, b.authority(false), &cancel).await;
    let mut reader = server.take_reader().unwrap();
    let pending = tokio::spawn(async move {
        let frame = reader.next().await.unwrap();
        (reader, frame)
    });
    let payload = serde_json::to_vec(&PeerFrame::Presence {
        presence: wire::Presence {
            known: vec!["alice/desktop/terminal".into()],
            ..Default::default()
        },
    })
    .unwrap();
    raw.send
        .write_all(&(payload.len() as u32).to_be_bytes())
        .await
        .unwrap();
    raw.send.write_all(&payload[..3]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!pending.is_finished());
    let mut outbound = message();
    std::mem::swap(&mut outbound.from, &mut outbound.to);
    server.send_text(outbound).await.unwrap();
    assert!(matches!(
        read_frame(&mut raw.receive, WAIT).await.unwrap(),
        Some(PeerFrame::Msg { .. })
    ));
    raw.send.write_all(&payload[3..]).await.unwrap();
    let (mut reader, token) = pending.await.unwrap();
    assert!(
        matches!(server.receive_frame(token).await.unwrap(), PeerEvent::Presence { peer, .. } if peer == left.id())
    );
    write_frame(
        &mut raw.send,
        &PeerFrame::Ack {
            id: "receipt-1".into(),
            state: "queued".into(),
            note: String::new(),
        },
        WAIT,
    )
    .await
    .unwrap();
    let token = reader.next().await.unwrap();
    assert!(
        matches!(server.receive_frame(token).await.unwrap(), PeerEvent::Queued { message_id } if message_id == "receipt-1")
    );
    drop((reader, server, raw));
    tokio::join!(left.shutdown(), right.shutdown());
}
#[tokio::test]
async fn complete_token_from_other_connection_cannot_borrow_current_peer_authority() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let authority = b.authority(false);
    let (mut raw, mut first) = raw_pair(&a, &left, &right, authority.clone(), &cancel).await;
    let (other_raw, mut other) = raw_pair(&a, &left, &right, authority, &cancel).await;
    let mut reader = first.take_reader().unwrap();
    write_frame(
        &mut raw.send,
        &PeerFrame::Presence {
            presence: wire::Presence::default(),
        },
        WAIT,
    )
    .await
    .unwrap();
    let token = reader.next().await.unwrap();
    assert!(matches!(
        other.receive_frame(token).await,
        Err(PeerError::Unauthorized)
    ));
    first.publish_state().await.unwrap();
    assert!(matches!(
        read_frame(&mut raw.receive, WAIT).await.unwrap(),
        Some(PeerFrame::State { .. })
    ));
    drop((reader, first, other, raw, other_raw));
    tokio::join!(left.shutdown(), right.shutdown());
}
