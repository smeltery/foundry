use super::*;
#[test]
fn connection_peeks_across_129_conversations_and_other_connections_remain_scoped() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let alice = register(&mut engine, 1, "alice", &root.0);
    let mut sessions = Vec::new();
    for index in 0..129 {
        let name = format!("bob{index}");
        let auth = register(&mut engine, 2, &name, &root.0);
        ok(
            &mut engine,
            1,
            send(&alice, &format!("m{index}"), &name, "x"),
        );
        sessions.push(auth);
    }
    for auth in &sessions[..128] {
        assert!(
            inbox(&mut engine, 2, auth, true)
                .unwrap()
                .attempt_id
                .is_some()
        );
    }
    let last = &sessions[128];
    let earlier = inbox(&mut engine, 2, last, true)
        .unwrap()
        .attempt_id
        .unwrap();
    assert_eq!(unread(&mut engine, 2, last), 1);
    ok(&mut engine, 3, Request::Attach { auth: last.clone() });
    let mail = inbox(&mut engine, 3, last, true).unwrap();
    assert_eq!(mail.message_id, "m128");
    ok(
        &mut engine,
        3,
        Request::Delivered {
            auth: last.clone(),
            attempt_id: mail.attempt_id.unwrap(),
        },
    );
    assert!(matches!(
        handle(
            &mut engine,
            2,
            Request::Delivered {
                auth: last.clone(),
                attempt_id: earlier
            }
        ),
        ReplyResult::Error(EngineError::StaleAttempt)
    ));
    engine.disconnect(ConnectionId(2)).unwrap();
    ok(
        &mut engine,
        2,
        Request::Attach {
            auth: sessions[0].clone(),
        },
    );
    assert!(
        inbox(&mut engine, 2, &sessions[0], true)
            .unwrap()
            .attempt_id
            .is_some()
    );
    assert_eq!(unread(&mut engine, 3, last), 0);
}
#[test]
fn maximum_identity_and_body_fit_stable_address_and_reply_frame() {
    fn is_send<T: Send>() {}
    is_send::<Engine>();
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let clock = time.clone();
    let mut engine = Engine::open_with_clock(
        &root.0,
        FolderPolicy::default(),
        TrustedIdentity {
            person: "p".repeat(512),
            machine: "m".repeat(512),
        },
        move || Ok(clock.load(Ordering::SeqCst)),
    )
    .unwrap();
    let auth = register(&mut engine, 1, &"n".repeat(512), &root.0);
    assert!(mailbox_address(&auth.conversation).len() <= 256);
    let request = send(
        &auth,
        "large",
        &auth.conversation.name,
        &"a".repeat(256 * 1024),
    );
    ok(&mut engine, 1, request);
    let reply = engine.handle(
        ConnectionId(1),
        RequestEnvelope {
            id: u64::MAX,
            request: Request::Inbox {
                wait_secs: 0,
                auth,
                peek: false,
            },
        },
    );
    let record = serde_json::to_vec(&reply).unwrap();
    assert!(record.len() < foundry_network::ipc_codec::MAX_IPC_BYTES);
    assert_eq!(
        serde_json::from_slice::<Reply>(&record).unwrap().id,
        u64::MAX
    );
    match reply.result {
        ReplyResult::Ok(Outcome::Inbox { messages, .. }) => {
            assert_eq!(messages.len(), 1);
            let mail = messages.into_iter().next().unwrap();
            assert_eq!(
                data_encoding::BASE64
                    .decode(mail.body_base64.as_bytes())
                    .unwrap()
                    .len(),
                256 * 1024
            );
        }
        _ => panic!("unexpected inbox reply"),
    }
}
#[test]
fn excluded_registration_and_attach_cannot_acquire_authority() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "a", &root.0);
    drop(engine);
    let policy = FolderPolicy::new(vec![(root.0.clone(), false)]).unwrap();
    let mut engine = Engine::open(
        &root.0,
        policy,
        TrustedIdentity {
            person: "person".into(),
            machine: "machine".into(),
        },
    )
    .unwrap();
    assert!(matches!(
        handle(&mut engine, 2, Request::Attach { auth }),
        ReplyResult::Error(EngineError::Excluded)
    ));
    assert!(matches!(
        handle(
            &mut engine,
            2,
            Request::Register {
                name: "b".into(),
                repository: root.0.clone(),
                share_path: true
            }
        ),
        ReplyResult::Error(EngineError::Excluded)
    ));
    match ok(
        &mut engine,
        2,
        Request::Who {
            filter: Filter::default(),
        },
    ) {
        Outcome::Directory { entries } => assert!(entries.is_empty()),
        _ => panic!("unexpected directory"),
    }
}
#[test]
fn late_peek_ack_is_valid_but_fails_after_consumption() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "a", &root.0);
    ok(&mut engine, 1, send(&auth, "late", "a", "text"));
    let mail = inbox(&mut engine, 1, &auth, true).unwrap();
    let first = mail.attempt_id.unwrap();
    assert!(mail.expires_ms.is_none());
    time.store(600_000, Ordering::SeqCst);
    ok(
        &mut engine,
        1,
        Request::Delivered {
            auth: auth.clone(),
            attempt_id: first,
        },
    );
    ok(&mut engine, 1, send(&auth, "next", "a", "text"));
    let second = inbox(&mut engine, 1, &auth, true)
        .unwrap()
        .attempt_id
        .unwrap();
    let read = inbox(&mut engine, 1, &auth, false).unwrap();
    assert!(matches!(read.state, MailState::Consumed));
    assert!(matches!(
        handle(
            &mut engine,
            1,
            Request::Delivered {
                auth,
                attempt_id: second
            }
        ),
        ReplyResult::Error(EngineError::StaleAttempt)
    ));
}
