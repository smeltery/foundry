use super::*;

async fn refused(frame: PeerFrame, expected: PeerError) {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (mut raw, mut server) = raw_pair(&a, &left, &right, b.authority(true), &cancel).await;
    let message_id = match &frame {
        PeerFrame::Msg { msg } => Some(msg.id.clone()),
        _ => None,
    };
    write_frame(&mut raw.send, &frame, WAIT).await.unwrap();
    if let Some(id) = message_id {
        assert!(
            matches!(server.receive().await.unwrap(), PeerEvent::Rejected {message_id,note} if message_id==id && note==expected.to_string())
        );
        assert!(
            matches!(read_frame(&mut raw.receive,WAIT).await.unwrap(),Some(PeerFrame::Ack {id:actual,state,note}) if actual==id && state=="refused" && note==expected.to_string())
        );
    } else {
        assert!(matches!(server.receive().await, Err(error) if error == expected));
        assert!(matches!(
            read_frame(&mut raw.receive, WAIT).await,
            Ok(None) | Err(_)
        ));
    }
    assert_eq!(
        mail_store::MailStore::open(&b.root)
            .unwrap()
            .unread("bob/desktop/terminal")
            .unwrap(),
        0
    );
    drop((raw, server));
    tokio::join!(left.shutdown(), right.shutdown());
}
#[tokio::test]
async fn presence_cannot_claim_another_namespace_and_lists_are_bounded() {
    refused(
        PeerFrame::Presence {
            presence: wire::Presence {
                known: vec!["bob/desktop/terminal".into()],
                ..Default::default()
            },
        },
        PeerError::Unauthorized,
    )
    .await;
    refused(
        PeerFrame::Presence {
            presence: wire::Presence {
                kept: vec!["alice/other-machine/terminal".into()],
                ..Default::default()
            },
        },
        PeerError::Unauthorized,
    )
    .await;
    refused(
        PeerFrame::Presence {
            presence: wire::Presence {
                known: vec!["alice/desktop/terminal".into(); MAX_PRESENCE + 1],
                ..Default::default()
            },
        },
        PeerError::Limit,
    )
    .await;
}
#[tokio::test]
async fn wire_sender_and_local_recipient_policy_are_independently_enforced() {
    let mut forged = message();
    forged.from = "bob/desktop/terminal".into();
    refused(
        PeerFrame::Msg {
            msg: Box::new(forged),
        },
        PeerError::Unauthorized,
    )
    .await;
    let mut wrong_machine = message();
    wrong_machine.to = "bob/laptop/terminal".into();
    refused(
        PeerFrame::Msg {
            msg: Box::new(wrong_machine),
        },
        PeerError::Unauthorized,
    )
    .await;
    let mut missing_session = message();
    missing_session.to = "bob/desktop/missing-session".into();
    refused(
        PeerFrame::Msg {
            msg: Box::new(missing_session),
        },
        PeerError::Unauthorized,
    )
    .await;
}
#[tokio::test]
async fn unsupported_attachments_are_refused_without_silent_stripping() {
    let mut with_file = message();
    with_file.files.push(wire::TextAttachment {
        name: "file.txt".into(),
        content: "text".into(),
        path: None,
    });
    refused(
        PeerFrame::Msg {
            msg: Box::new(with_file),
        },
        PeerError::Unsupported,
    )
    .await;
    let mut with_route = message();
    with_route.approval = Some(wire::SignedDocument {
        body: serde_json::json!({"fixture":true}),
        by: "fixture-signer".into(),
        sig: "fixture-signature".into(),
    });
    refused(
        PeerFrame::Msg {
            msg: Box::new(with_route),
        },
        PeerError::Unsupported,
    )
    .await;
}
#[tokio::test]
async fn unexpected_ack_and_conflicting_message_ids_never_settle_other_mail() {
    refused(
        PeerFrame::Ack {
            id: "unowned-id".into(),
            state: "queued".into(),
            note: String::new(),
        },
        PeerError::UnexpectedAck,
    )
    .await;
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (mut raw, mut server) = raw_pair(&a, &left, &right, b.authority(true), &cancel).await;
    write_frame(
        &mut raw.send,
        &PeerFrame::Msg {
            msg: Box::new(message()),
        },
        WAIT,
    )
    .await
    .unwrap();
    assert!(
        matches!(server.receive().await.unwrap(), PeerEvent::Received { duplicate: false, message_id } if message_id == "receipt-1")
    );
    assert!(matches!(
        read_frame(&mut raw.receive, WAIT).await.unwrap(),
        Some(PeerFrame::Ack { .. })
    ));
    let mut conflicting = message();
    conflicting.body = "replacement".into();
    write_frame(
        &mut raw.send,
        &PeerFrame::Msg {
            msg: Box::new(conflicting),
        },
        WAIT,
    )
    .await
    .unwrap();
    assert!(matches!(
        server.receive().await.unwrap(),
        PeerEvent::Rejected { .. }
    ));
    assert!(
        matches!(read_frame(&mut raw.receive,WAIT).await.unwrap(),Some(PeerFrame::Ack {state,..}) if state=="refused")
    );
    assert_eq!(
        mail_store::MailStore::open(&b.root)
            .unwrap()
            .unread("bob/desktop/terminal")
            .unwrap(),
        1
    );
    drop((raw, server));
    tokio::join!(left.shutdown(), right.shutdown());
}
#[tokio::test]
async fn receiver_storage_failure_sends_no_success_ack_and_fences_authority() {
    use std::os::unix::fs::PermissionsExt;
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let authority = b.authority(true);
    let (mut raw, mut server) = raw_pair(&a, &left, &right, authority.clone(), &cancel).await;
    std::fs::set_permissions(
        b.root.join("mail.sqlite3"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    write_frame(
        &mut raw.send,
        &PeerFrame::Msg {
            msg: Box::new(message()),
        },
        WAIT,
    )
    .await
    .unwrap();
    assert!(matches!(server.receive().await, Err(PeerError::Storage)));
    assert!(matches!(
        read_frame(&mut raw.receive, WAIT).await,
        Ok(None) | Err(_)
    ));
    assert_eq!(authority.refresh().await, Err(PeerError::Quiescent));
    drop((raw, server, authority));
    tokio::join!(left.shutdown(), right.shutdown());
}

#[tokio::test]
async fn invalid_signed_state_cannot_mutate_or_advertise_a_candidate() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (mut raw, mut server) = raw_pair(&a, &left, &right, b.authority(false), &cancel).await;
    let before = b.store().load(NOW).unwrap().document().unwrap().clone();
    let mut incoming = a.store().load(NOW).unwrap().document().unwrap().clone();
    incoming.meta.body.ts = NOW + 1;
    incoming.meta.body.name = "unsigned-change".into();
    let state = serde_json::from_value(serde_json::to_value(incoming).unwrap()).unwrap();
    write_frame(
        &mut raw.send,
        &PeerFrame::State {
            state: Box::new(state),
        },
        WAIT,
    )
    .await
    .unwrap();
    assert!(matches!(server.receive().await, Err(PeerError::Invalid)));
    assert!(b.store().load(NOW + 100).unwrap().document() == Some(&before));
    assert!(matches!(
        read_frame(&mut raw.receive, WAIT).await,
        Ok(None) | Err(_)
    ));
    drop((raw, server));
    tokio::join!(left.shutdown(), right.shutdown());
}

#[tokio::test]
async fn queued_ack_cannot_cross_a_durable_membership_revision() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (mut raw, mut server) = raw_pair(&a, &left, &right, b.authority(false), &cancel).await;
    let mut outgoing = message();
    std::mem::swap(&mut outgoing.from, &mut outgoing.to);
    server.send_text(outgoing).await.unwrap();
    assert!(matches!(
        read_frame(&mut raw.receive, WAIT).await.unwrap(),
        Some(PeerFrame::Msg { .. })
    ));
    let mut store = b.store();
    let base = store.load(NOW).unwrap();
    let pending = store.prepare_rotation(&base, NOW + 2).unwrap();
    store.commit_transition(pending, NOW + 2).unwrap();
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
    assert!(matches!(server.receive().await, Err(PeerError::StaleEpoch)));
    drop((raw, server, store));
    tokio::join!(left.shutdown(), right.shutdown());
}

#[tokio::test]
async fn join_alpn_and_invalid_frame_lengths_still_refused() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (client, server) = tokio::join!(
        left.connect(right.address(), Channel::Join, WAIT),
        PeerSession::accept(&right, b.authority(false), options(&cancel))
    );
    assert!(matches!(server, Err(PeerError::Protocol)));
    if let Ok(client) = client {
        client.close(0u32.into(), b"fixture finished");
    }
    let (mut raw, mut server) = raw_pair(&a, &left, &right, b.authority(false), &cancel).await;
    raw.send.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
    assert!(matches!(server.receive().await, Err(PeerError::Invalid)));
    drop((raw, server));
    tokio::join!(left.shutdown(), right.shutdown());
}
