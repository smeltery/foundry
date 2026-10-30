use super::*;
#[test]
fn nonpeek_consumes_without_injection_and_retains_body_across_restart() {
    let root = Root::new();
    let mut store = root.open();
    let original = message("read");
    store.insert(&original, State::Pending).unwrap();
    let read = store
        .poll_batch("bob", "s", 1, false)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert!(read.attempt.is_none());
    assert_eq!(read.message, original);
    assert_eq!(store.unread("bob").unwrap(), 0);
    assert_eq!(store.get("read").unwrap().state, State::Consumed);
    assert_eq!(
        store.insert(&original, State::Pending).unwrap(),
        Insert::Duplicate
    );
    drop(store);
    let mut store = root.open();
    assert!(store.poll_batch("bob", "s", 2, false).unwrap().is_empty());
    assert_eq!(store.get("read").unwrap().message, original);
    assert_eq!(store.outbox("alice", 10).unwrap()[0].state, State::Consumed);
}
#[test]
fn peek_tokens_do_not_hide_pending_or_spend_retry_budget() {
    let root = Root::new();
    let mut store = root.open();
    store.insert(&message("peek"), State::Pending).unwrap();
    let first = store
        .poll_batch("bob", "s1", 0, true)
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .attempt
        .unwrap();
    for _ in 0..140 {
        assert_eq!(
            store
                .poll_batch("bob", "s1", 0, true)
                .unwrap()
                .into_iter()
                .next()
                .unwrap()
                .attempt
                .unwrap()
                .id(),
            first.id()
        );
    }
    let other = store
        .poll_batch("bob", "s2", 0, true)
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .attempt
        .unwrap();
    assert_ne!(first.id(), other.id());
    assert_eq!(store.unread("bob").unwrap(), 1);
    assert_eq!(store.get("peek").unwrap().retry.attempts, 0);
    assert!(matches!(
        store.acknowledge(&first, "s2", 1),
        Err(MailError::StaleAttempt)
    ));
    assert_eq!(store.disconnect("s1").unwrap(), 1);
    assert!(matches!(
        store.acknowledge(&first, "s1", 1),
        Err(MailError::StaleAttempt)
    ));
    store.fail(&other, "s2", 1).unwrap();
    let new = store
        .poll_batch("bob", "s2", 1, true)
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .attempt
        .unwrap();
    store.acknowledge(&new, "s2", 2).unwrap();
    assert_eq!(store.get("peek").unwrap().state, State::Delivered);
}
#[test]
fn polling_respects_live_push_but_not_its_backoff_or_exhaustion() {
    let root = Root::new();
    let mut store = root.open();
    store.insert(&message("push"), State::Pending).unwrap();
    let delivery = store.claim("bob", "push-adapter", 0, 10).unwrap().unwrap();
    assert!(store.poll_batch("bob", "poll", 1, true).unwrap().is_empty());
    store.fail(&delivery.attempt, "push-adapter", 1).unwrap();
    let peek = store
        .poll_batch("bob", "poll", 1, true)
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .attempt
        .unwrap();
    store
        .poll_batch("bob", "other", 1, false)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert!(matches!(
        store.acknowledge(&peek, "poll", 2),
        Err(MailError::StaleAttempt)
    ));
    assert!(matches!(
        store.acknowledge(&delivery.attempt, "push-adapter", 2),
        Err(MailError::StaleAttempt)
    ));
    assert_eq!(store.get("push").unwrap().state, State::Consumed);
}
#[test]
fn exact_v2_migration_preserves_rows_intents_retry_and_sequence() {
    use std::os::unix::fs::PermissionsExt;
    let root = Root::new();
    let path = root.0.join("mail.sqlite3");
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch("CREATE TABLE mail(id TEXT PRIMARY KEY NOT NULL,sender TEXT NOT NULL,recipient TEXT NOT NULL,body BLOB NOT NULL,reply_to TEXT,wake INTEGER NOT NULL CHECK(wake IN (0,1)),notify INTEGER NOT NULL CHECK(notify IN (0,1)),state INTEGER NOT NULL CHECK(state BETWEEN 0 AND 3),tries INTEGER NOT NULL DEFAULT 0 CHECK(tries BETWEEN 0 AND 8),next_ms INTEGER NOT NULL DEFAULT 0 CHECK(next_ms>=0),attempt INTEGER,session TEXT,expires INTEGER,CHECK((attempt IS NULL AND session IS NULL AND expires IS NULL) OR (state=0 AND attempt IS NOT NULL AND session IS NOT NULL AND expires IS NOT NULL))); CREATE TABLE sequence(id INTEGER PRIMARY KEY CHECK(id=1),value INTEGER NOT NULL CHECK(value>=0)); INSERT INTO sequence VALUES(1,42); PRAGMA application_id=1179468108; PRAGMA user_version=2;").unwrap();
    for (id, state) in [("p", 0), ("h", 1), ("r", 2), ("d", 3)] {
        raw.execute("INSERT INTO mail(id,sender,recipient,body,reply_to,wake,notify,state,tries,next_ms) VALUES(?1,'alice','bob',?2,'thread',0,1,?3,8,9000)", rusqlite::params![id,b"retained".as_slice(),state]).unwrap();
    }
    raw.execute("INSERT INTO mail(id,sender,recipient,body,wake,notify,state,tries,next_ms,attempt,session,expires) VALUES('flight','alice','offline',?1,1,0,0,3,2000,41,'old-scope',1000)", [b"in flight".as_slice()]).unwrap();
    raw.execute("INSERT INTO mail(id,sender,recipient,body,wake,notify,state) VALUES('literal','alice','local-old',?1,1,0,0)", [br#"{"body":"literal legacy JSON"}"#.as_slice()]).unwrap();
    drop(raw);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut store = root.open();
    let legacy = store.get("literal").unwrap();
    assert_eq!(
        legacy.message.encoding,
        foundry_network::mail_store::PayloadEncoding::Text
    );
    assert_eq!(
        legacy.message.display_body().unwrap(),
        br#"{"body":"literal legacy JSON"}"#
    );
    assert_eq!(store.unread("bob").unwrap(), 1);
    for (id, state) in [
        ("p", State::Pending),
        ("h", State::Held),
        ("r", State::Rejected),
        ("d", State::Delivered),
    ] {
        let row = store.get(id).unwrap();
        assert_eq!(row.state, state);
        assert_eq!(
            row.message.encoding,
            foundry_network::mail_store::PayloadEncoding::Text
        );
        assert_eq!(row.message.body, b"retained");
        assert_eq!(row.message.reply_to.as_deref(), Some("thread"));
        assert!(!row.message.intent.wake);
        assert!(row.message.intent.notify);
        assert_eq!(row.retry.attempts, 8);
        assert_eq!(row.retry.next_ms, 9000);
    }
    assert!(
        store
            .poll_batch("offline", "fresh", 999, true)
            .unwrap()
            .is_empty()
    );
    let raw = rusqlite::Connection::open(&path).unwrap();
    let old: (i64, String, i64) = raw
        .query_row(
            "SELECT attempt,session,expires FROM mail WHERE id='flight'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(old, (41, "old-scope".into(), 1000));
    drop(raw);
    let token = store
        .poll_batch("bob", "s", 0, true)
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .attempt
        .unwrap();
    assert_eq!(token.id(), 43);
    store.poll_batch("bob", "s", 0, false).unwrap();
    assert!(matches!(
        store.resume("p", 1),
        Err(MailError::InvalidTransition)
    ));
    assert!(
        !store
            .poll_batch("offline", "fresh", 1000, true)
            .unwrap()
            .is_empty()
    );
    drop(store);
    assert_eq!(root.open().get("p").unwrap().state, State::Consumed);
    let raw = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        raw.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
    assert_eq!(
        raw.query_row("SELECT count(*) FROM mail", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        6
    );
}
#[test]
fn concurrent_nonpeek_reads_consume_once_without_deleting_mail() {
    let root = Root::new();
    root.open().insert(&message("one"), State::Pending).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|i| {
            let path = root.0.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = MailStore::open(&path).unwrap();
                barrier.wait();
                !store
                    .poll_batch("bob", &format!("s{i}"), 0, false)
                    .unwrap()
                    .is_empty()
            })
        })
        .collect();
    assert_eq!(
        workers
            .into_iter()
            .map(|h| usize::from(h.join().unwrap()))
            .sum::<usize>(),
        1
    );
    assert_eq!(root.open().get("one").unwrap().state, State::Consumed);
}
