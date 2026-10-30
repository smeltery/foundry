use super::*;
use std::io::{self, Write};

#[derive(Default)]
struct Output<'a> {
    bytes: Vec<u8>,
    fail_after: Option<usize>,
    fail_flush: bool,
    fail_seal: bool,
    sealed: bool,
    cancel: Option<&'a InboxWait>,
    probe: Option<&'a rusqlite::Connection>,
}
impl Write for Output<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .fail_after
            .is_some_and(|n| self.bytes.len() + bytes.len() > n)
        {
            return Err(io::Error::other("test write failure"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            Err(io::Error::other("test flush failure"))
        } else {
            Ok(())
        }
    }
}
impl InboxOutput for Output<'_> {
    fn seal(&mut self) -> io::Result<()> {
        if self.fail_seal {
            return Err(io::Error::other("test seal failure"));
        }
        if let Some(raw) = self.probe {
            assert_eq!(
                raw.query_row(
                    "SELECT count(*) FROM mail WHERE read_ms IS NOT NULL",
                    [],
                    |r| r.get::<_, u64>(0)
                )
                .unwrap(),
                0
            );
            assert_eq!(
                raw.query_row("SELECT value FROM sequence", [], |r| r.get::<_, u64>(0))
                    .unwrap(),
                0
            );
        }
        if let Some(wait) = self.cancel {
            wait.cancel();
        }
        self.sealed = true;
        Ok(())
    }
}
fn parse(output: &Output<'_>, id: u64, count: usize) -> (Vec<Mail>, String) {
    assert!(output.sealed);
    assert_eq!(output.bytes.last(), Some(&b'\n'));
    let reply: Reply = serde_json::from_slice(&output.bytes).unwrap();
    assert_eq!(reply.id, id);
    match reply.result {
        ReplyResult::Ok(Outcome::Inbox { messages, text }) => {
            assert_eq!(messages.len(), count);
            (messages, text)
        }
        _ => panic!("not inbox"),
    }
}
#[test]
fn incremental_snapshot_large_reply_peek_ack_scope_and_restart() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "a", &root.0);
    let other = register(&mut engine, 2, "b", &root.0);
    let body = "x\0\n\"\\é".repeat(4000);
    for i in 0..129 {
        ok(&mut engine, 1, send(&auth, &format!("m{i}"), "a", &body));
    }
    ok(
        &mut engine,
        2,
        send(&other, "other", "b", "literal {\"body\":1}"),
    );
    let mut output = Output::default();
    let summary = engine
        .inbox_to_output(ConnectionId(1), &auth, u64::MAX, true, &mut output)
        .unwrap()
        .unwrap();
    assert_eq!(summary.messages, 129);
    assert_eq!(summary.bytes as usize, output.bytes.len());
    assert!(summary.bytes > 1024 * 1024);
    let (messages, text) = parse(&output, u64::MAX, 129);
    assert_eq!(text.matches(&body).count(), 129);
    assert_eq!(
        data_encoding::BASE64
            .decode(messages[0].body_base64.as_bytes())
            .unwrap(),
        body.as_bytes()
    );
    let token = messages[0].attempt_id.unwrap();
    ok(&mut engine, 3, Request::Attach { auth: auth.clone() });
    assert!(matches!(
        handle(
            &mut engine,
            3,
            Request::Delivered {
                auth: auth.clone(),
                attempt_id: token
            }
        ),
        ReplyResult::Error(EngineError::StaleAttempt)
    ));
    ok(
        &mut engine,
        1,
        Request::Delivered {
            auth: auth.clone(),
            attempt_id: token,
        },
    );
    assert_eq!(unread(&mut engine, 1, &auth), 128);
    assert_eq!(unread(&mut engine, 2, &other), 1);
    drop(engine);
    let mut engine = open(&root, &time);
    ok(&mut engine, 4, Request::Attach { auth: auth.clone() });
    assert!(matches!(
        handle(
            &mut engine,
            4,
            Request::Delivered {
                auth: auth.clone(),
                attempt_id: messages[1].attempt_id.unwrap()
            }
        ),
        ReplyResult::Error(EngineError::StaleAttempt)
    ));
    let mut output = Output::default();
    engine
        .inbox_to_output(ConnectionId(4), &auth, 9, false, &mut output)
        .unwrap()
        .unwrap();
    let (messages, _) = parse(&output, 9, 128);
    assert!(
        messages
            .iter()
            .all(|m| matches!(m.state, MailState::Consumed))
    );
    assert_eq!(unread(&mut engine, 4, &auth), 0);
    assert_eq!(
        MailStore::open(&root.0).unwrap().get("m0").unwrap().state,
        State::Delivered
    );
    assert_eq!(
        MailStore::open(&root.0).unwrap().get("m1").unwrap().state,
        State::Consumed
    );
}
#[test]
fn write_flush_seal_and_final_cancellation_roll_back_all_changes() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "a", &root.0);
    for id in ["one", "two"] {
        ok(&mut engine, 1, send(&auth, id, "a", "body"));
    }
    let raw = rusqlite::Connection::open(root.0.join("mail.sqlite3")).unwrap();
    for peek in [false, true] {
        for failure in 0..3 {
            let mut output = Output {
                fail_after: (failure == 0).then_some(200),
                fail_flush: failure == 1,
                fail_seal: failure == 2,
                ..Output::default()
            };
            assert!(matches!(
                engine.inbox_to_output(ConnectionId(1), &auth, 3, peek, &mut output),
                Err(EngineError::Output)
            ));
            assert_eq!(unread(&mut engine, 1, &auth), 2);
            assert_eq!(
                raw.query_row("SELECT count(*) FROM poll_attempt", [], |r| r
                    .get::<_, u64>(0))
                    .unwrap(),
                0
            );
            assert_eq!(
                raw.query_row("SELECT value FROM sequence", [], |r| r.get::<_, u64>(0))
                    .unwrap(),
                0
            );
        }
        let wait = engine.begin_inbox_wait(ConnectionId(1), &auth).unwrap();
        let mut output = Output {
            cancel: Some(&wait),
            probe: Some(&raw),
            ..Output::default()
        };
        assert!(
            engine
                .poll_inbox_wait_to_output(&wait, 4, peek, &mut output)
                .unwrap()
                .is_none()
        );
        assert!(output.sealed);
        assert_eq!(unread(&mut engine, 1, &auth), 2);
        assert_eq!(
            raw.query_row("SELECT value FROM sequence", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
    let mut output = Output {
        probe: Some(&raw),
        ..Output::default()
    };
    engine
        .inbox_to_output(ConnectionId(1), &auth, 5, false, &mut output)
        .unwrap()
        .unwrap();
    parse(&output, 5, 2);
    assert_eq!(unread(&mut engine, 1, &auth), 0);
}

#[test]
fn incremental_json_exactly_matches_typed_reply_with_fragment_boundaries() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "a", &root.0);
    for (i, body) in ["a", "ab", "abc", "\0\t\n\r\u{1f}\"\\é😀", "{\"body\":true}"]
        .iter()
        .enumerate()
    {
        ok(&mut engine, 1, send(&auth, &format!("q{i}"), "a", body));
    }
    let sql = rusqlite::Connection::open(root.0.join("mail.sqlite3")).unwrap();
    sql.execute(
        "UPDATE mail SET sender=?1,reply_to=?1",
        ["\"\\".repeat(128)],
    )
    .unwrap();
    let typed = engine.handle(
        ConnectionId(1),
        RequestEnvelope {
            id: u64::MAX,
            request: Request::Inbox {
                auth: auth.clone(),
                peek: true,
                wait_secs: 0,
            },
        },
    );
    let mut expected = serde_json::to_vec(&typed).unwrap();
    expected.push(b'\n');
    let mut output = Output::default();
    engine
        .inbox_to_output(ConnectionId(1), &auth, u64::MAX, true, &mut output)
        .unwrap()
        .unwrap();
    assert_eq!(output.bytes, expected);
    assert!(output.bytes.len() < MAX_INBOX_REPLY_BYTES);
    assert_eq!(unread(&mut engine, 1, &auth), 5);
    let empty = register(&mut engine, 2, "empty", &root.0);
    let mut output = Output::default();
    let summary = engine
        .inbox_to_output(ConnectionId(2), &empty, 0, false, &mut output)
        .unwrap()
        .unwrap();
    assert_eq!(summary.messages, 0);
    assert_eq!(summary.bytes as usize, output.bytes.len());
    assert_eq!(parse(&output, 0, 0).1, "");
}

#[test]
fn later_invalid_display_and_corrupt_metadata_never_commit_prefix() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "a", &root.0);
    for id in ["one", "two"] {
        ok(&mut engine, 1, send(&auth, id, "a", "body"));
    }
    let raw = rusqlite::Connection::open(root.0.join("mail.sqlite3")).unwrap();
    for update in [
        "UPDATE mail SET body=x'ff' WHERE id='two'",
        "UPDATE mail SET encoding=1 WHERE id='two'",
        "UPDATE mail SET sender=char(1) WHERE id='two'",
    ] {
        raw.execute(update, []).unwrap();
        for peek in [false, true] {
            let mut output = Output::default();
            assert!(
                engine
                    .inbox_to_output(ConnectionId(1), &auth, 3, peek, &mut output)
                    .is_err()
            );
            assert!(!output.sealed);
            assert_eq!(unread(&mut engine, 1, &auth), 2);
            assert_eq!(
                raw.query_row("SELECT value FROM sequence", [], |r| r.get::<_, u64>(0))
                    .unwrap(),
                0
            );
        }
        raw.execute(
            "UPDATE mail SET body=x'626f6479',encoding=0,sender='sender' WHERE id='two'",
            [],
        )
        .unwrap();
    }
}

#[test]
fn immediate_read_guard_cancels_without_changing_positive_polling_activity() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "a", &root.0);
    let read = engine.begin_inbox_read(ConnectionId(1), &auth).unwrap();
    assert!(!engine.is_polling(&auth.conversation));
    let wait = engine.begin_inbox_wait(ConnectionId(1), &auth).unwrap();
    assert!(engine.is_polling(&auth.conversation));
    drop(read);
    assert!(engine.is_polling(&auth.conversation));
    let read = engine.begin_inbox_read(ConnectionId(1), &auth).unwrap();
    ok(&mut engine, 1, Request::StopWaiting { auth: auth.clone() });
    assert!(read.cancelled());
    assert!(wait.cancelled());
    let mut output = Output::default();
    assert!(
        engine
            .poll_inbox_wait_to_output(&read, 1, false, &mut output)
            .unwrap()
            .is_none()
    );
    assert!(!output.sealed);
    assert!(output.bytes.is_empty());
    drop(wait);
    assert!(!engine.is_polling(&auth.conversation));
    drop(read);
    assert!(!engine.is_polling(&auth.conversation));
}
