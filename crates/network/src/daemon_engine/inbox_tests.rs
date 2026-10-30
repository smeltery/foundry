use super::*;
#[test]
fn default_read_is_consumed_not_injected_and_restart_retains_history() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "a", &root.0);
    ok(&mut engine, 1, send(&auth, "read", "a", "body"));
    let value =
        serde_json::json!({"id":99,"op":"inbox","auth":serde_json::to_value(&auth).unwrap()});
    let reply = engine.handle(ConnectionId(1), serde_json::from_value(value).unwrap());
    match reply.result {
        ReplyResult::Ok(Outcome::Inbox { messages, .. }) => {
            assert_eq!(messages.len(), 1);
            let mail = messages.into_iter().next().unwrap();
            assert!(matches!(mail.state, MailState::Consumed));
            assert!(mail.attempt_id.is_none());
        }
        _ => panic!("missing read"),
    }
    assert_eq!(unread(&mut engine, 1, &auth), 0);
    engine.disconnect(ConnectionId(1)).unwrap();
    drop(engine);
    let mut engine = open(&root, &time);
    ok(&mut engine, 2, Request::Attach { auth: auth.clone() });
    assert!(inbox(&mut engine, 2, &auth, false).is_none());
    let stored = MailStore::open(&root.0).unwrap().get("read").unwrap();
    assert_eq!(stored.state, State::Consumed);
    assert_eq!(stored.message.body, b"body");
    assert!(matches!(
        ok(&mut engine, 2, send(&auth, "read", "a", "body")),
        Outcome::Sent {
            duplicate: true,
            state: MailState::Consumed,
            ..
        }
    ));
}
#[test]
fn cancellation_is_exact_conversation_and_polling_gate_drops_with_guard() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let a = register(&mut engine, 1, "a", &root.0);
    let b = register(&mut engine, 2, "b", &root.0);
    let first = engine.begin_inbox_wait(ConnectionId(1), &a).unwrap();
    let second = engine.begin_inbox_wait(ConnectionId(2), &b).unwrap();
    assert!(engine.is_polling(&a.conversation));
    assert!(engine.is_polling(&b.conversation));
    let forged = Auth {
        conversation: b.conversation.clone(),
        credential: a.credential,
    };
    assert!(matches!(
        handle(&mut engine, 1, Request::StopWaiting { auth: forged }),
        ReplyResult::Error(EngineError::Unauthorized)
    ));
    ok(&mut engine, 1, Request::StopWaiting { auth: a.clone() });
    assert!(first.cancelled());
    assert!(!second.cancelled());
    ok(&mut engine, 1, send(&a, "pending", "a", "body"));
    assert!(matches!(
        engine.poll_inbox_wait(&first, 9, false).result,
        ReplyResult::Ok(Outcome::Inbox { ref messages, .. }) if messages.is_empty()
    ));
    assert_eq!(unread(&mut engine, 1, &a), 1);
    drop(first);
    assert!(!engine.is_polling(&a.conversation));
    assert!(engine.is_polling(&b.conversation));
    drop(second);
    assert!(!engine.is_polling(&b.conversation));
    assert!(inbox(&mut engine, 1, &a, true).is_some());
}
#[test]
fn positive_wait_cannot_be_silently_ignored_by_sync_handler() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "a", &root.0);
    assert_eq!(foundry_network::daemon_engine::MAX_WAIT_SECS, 600);
    assert!(matches!(
        handle(
            &mut engine,
            1,
            Request::Inbox {
                auth: auth.clone(),
                wait_secs: u64::MAX,
                peek: false
            }
        ),
        ReplyResult::Error(EngineError::AsyncRequired)
    ));
    assert!(matches!(
        handle(
            &mut engine,
            2,
            Request::Inbox {
                auth,
                wait_secs: 1,
                peek: false
            }
        ),
        ReplyResult::Error(EngineError::Unauthorized)
    ));
}

#[test]
fn local_json_looking_message_remains_literal_in_engine_display() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "a", &root.0);
    let text = r#"{"body":"do not extract this","sent_at":200}"#;
    ok(&mut engine, 1, send(&auth, "literal", "a", text));
    let mail = inbox(&mut engine, 1, &auth, false).unwrap();
    assert_eq!(
        data_encoding::BASE64
            .decode(mail.body_base64.as_bytes())
            .unwrap(),
        text.as_bytes()
    );
    assert_eq!(
        MailStore::open(&root.0)
            .unwrap()
            .get("literal")
            .unwrap()
            .message
            .encoding,
        foundry_network::mail_store::PayloadEncoding::Text
    );
}
