use super::*;
fn batch(engine: &mut Engine, connection: u64, auth: &Auth, peek: bool) -> Vec<Mail> {
    match ok(
        engine,
        connection,
        Request::Inbox {
            auth: auth.clone(),
            peek,
            wait_secs: 0,
        },
    ) {
        Outcome::Inbox { messages, text } => {
            assert_eq!(messages.is_empty(), text.is_empty());
            messages
        }
        _ => panic!("not an inbox"),
    }
}
#[test]
fn batch_peek_subset_ack_and_default_read_are_scoped() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let a = register(&mut engine, 1, "a", &root.0);
    let b = register(&mut engine, 2, "b", &root.0);
    for id in ["one", "two"] {
        ok(&mut engine, 1, send(&a, id, "b", id));
    }
    ok(&mut engine, 1, send(&a, "other", "a", "other"));
    let messages = batch(&mut engine, 2, &b, true);
    assert_eq!(messages.len(), 2);
    assert_eq!(unread(&mut engine, 2, &b), 2);
    ok(
        &mut engine,
        2,
        Request::Delivered {
            auth: b.clone(),
            attempt_id: messages[0].attempt_id.unwrap(),
        },
    );
    let remaining = batch(&mut engine, 2, &b, true);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].message_id, "two");
    assert_eq!(remaining[0].attempt_id, messages[1].attempt_id);
    let read = batch(&mut engine, 2, &b, false);
    assert_eq!(read.len(), 1);
    assert!(matches!(read[0].state, MailState::Consumed));
    assert_eq!(unread(&mut engine, 1, &a), 1);
    assert_eq!(unread(&mut engine, 2, &b), 0);
    assert_eq!(
        MailStore::open(&root.0).unwrap().get("one").unwrap().state,
        State::Delivered
    );
    assert_eq!(
        MailStore::open(&root.0).unwrap().get("two").unwrap().state,
        State::Consumed
    );
}
#[test]
fn actual_serialized_reply_budget_rejects_batch_before_consumption() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "a", &root.0);
    for n in 0..3 {
        ok(
            &mut engine,
            1,
            send(&auth, &format!("m{n}"), "a", &"a".repeat(150 * 1024)),
        );
    }
    for peek in [false, true] {
        assert!(matches!(
            handle(
                &mut engine,
                1,
                Request::Inbox {
                    auth: auth.clone(),
                    peek,
                    wait_secs: 0
                }
            ),
            ReplyResult::Error(EngineError::Limit)
        ));
        assert_eq!(unread(&mut engine, 1, &auth), 3);
    }
    let raw = rusqlite::Connection::open(root.0.join("mail.sqlite3")).unwrap();
    assert_eq!(
        raw.query_row("SELECT count(*) FROM poll_attempt", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        raw.query_row("SELECT value FROM sequence", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn escaping_and_rendered_text_count_toward_frame_limit() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "a", &root.0);
    ok(
        &mut engine,
        1,
        send(&auth, "escaped", "a", &"\0".repeat(256 * 1024)),
    );
    assert!(matches!(
        handle(
            &mut engine,
            1,
            Request::Inbox {
                auth: auth.clone(),
                peek: false,
                wait_secs: 0
            }
        ),
        ReplyResult::Error(EngineError::Limit)
    ));
    assert_eq!(unread(&mut engine, 1, &auth), 1);
}
