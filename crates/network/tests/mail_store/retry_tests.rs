use super::*;
#[test]
fn reply_and_intents_are_immutable_and_persisted() {
    let root = Root::new();
    let mut db = root.open();
    let mail = Message {
        reply_to: Some("thread-root".into()),
        ..message("reply")
    };
    assert!(mail.intent.wake);
    assert!(!mail.intent.notify);
    db.insert(&mail, State::Pending).unwrap();
    drop(db);
    let mut db = root.open();
    assert_eq!(db.get("reply").unwrap().message, mail);
    assert_eq!(db.insert(&mail, State::Pending).unwrap(), Insert::Duplicate);
    for different in [
        Message {
            reply_to: None,
            ..mail.clone()
        },
        Message {
            reply_to: Some("other-root".into()),
            ..mail.clone()
        },
        Message {
            intent: DeliveryIntent {
                wake: false,
                notify: false,
            },
            ..mail.clone()
        },
        Message {
            intent: DeliveryIntent {
                wake: true,
                notify: true,
            },
            ..mail.clone()
        },
    ] {
        assert!(matches!(
            db.insert(&different, State::Pending),
            Err(MailError::Conflict)
        ));
    }
    let quiet = Message {
        intent: DeliveryIntent {
            wake: false,
            notify: true,
        },
        ..message("quiet")
    };
    db.insert(&quiet, State::Pending).unwrap();
    let delivery = db.claim("bob", "s", 0, 10).unwrap().unwrap();
    assert_eq!(delivery.mail, mail);
    db.acknowledge(&delivery.attempt, "s", 1).unwrap();
    drop(db);
    let mut db = root.open();
    assert_eq!(db.claim("bob", "s", 2, 10).unwrap().unwrap().mail, quiet);
    assert_eq!(
        db.outbox("alice", 10).unwrap()[0].message.reply_to,
        mail.reply_to
    );
    let invalid = Message {
        reply_to: Some("x".repeat(257)),
        ..message("invalid")
    };
    assert!(matches!(
        db.insert(&invalid, State::Pending),
        Err(MailError::InvalidInput)
    ));
}
#[test]
fn failures_back_off_exhaust_and_resume_without_losing_mail() {
    let root = Root::new();
    let mut db = root.open();
    db.insert(&message("retry"), State::Pending).unwrap();
    let mut now = 0;
    let delays = [1000, 2000, 4000, 8000, 16000, 32000, 60000, 60000];
    let mut last = None;
    for (index, delay) in delays.into_iter().enumerate() {
        let delivery = db.claim("bob", "s", now, 10).unwrap().unwrap();
        assert!(db.resume("retry", now).is_err());
        if let Some(stale) = &last {
            assert!(matches!(
                db.acknowledge(stale, "s", now),
                Err(MailError::StaleAttempt)
            ));
        }
        db.fail(&delivery.attempt, "s", now + 1).unwrap();
        assert!(matches!(
            db.fail(&delivery.attempt, "s", now + 1),
            Err(MailError::StaleAttempt)
        ));
        let status = db.get("retry").unwrap().retry;
        assert_eq!(status.attempts, index as u32 + 1);
        assert_eq!(status.next_ms, now + 1 + delay);
        assert!(
            db.claim("bob", "s", status.next_ms - 1, 10)
                .unwrap()
                .is_none()
        );
        assert_eq!(db.unread("bob").unwrap(), 1);
        last = Some(delivery.attempt);
        now = status.next_ms;
        drop(db);
        db = root.open();
        assert_eq!(db.get("retry").unwrap().retry, status);
    }
    assert!(db.get("retry").unwrap().retry.exhausted());
    assert!(db.claim("bob", "s", now + 1_000_000, 10).unwrap().is_none());
    assert_eq!(db.peek("bob", 10).unwrap(), vec![message("retry")]);
    assert_eq!(db.outbox("alice", 10).unwrap()[0].state, State::Pending);
    db.resume("retry", now).unwrap();
    let new = db.claim("bob", "s", now, 10).unwrap().unwrap();
    assert_ne!(new.attempt.id(), last.as_ref().unwrap().id());
    assert!(matches!(
        db.acknowledge(last.as_ref().unwrap(), "s", now),
        Err(MailError::StaleAttempt)
    ));
    db.acknowledge(&new.attempt, "s", now + 1).unwrap();
    assert_eq!(db.get("retry").unwrap().state, State::Delivered);
    assert!(db.resume("retry", now + 2).is_err());
}
#[test]
fn lost_leases_and_disconnects_also_consume_retry_budget() {
    let root = Root::new();
    let mut db = root.open();
    db.insert(&message("lost"), State::Pending).unwrap();
    let mut now = 0;
    for i in 0..MAX_DELIVERY_ATTEMPTS {
        let delivery = db.claim("bob", "s", now, 10).unwrap().unwrap();
        if i % 2 == 0 {
            assert_eq!(db.disconnect("s").unwrap(), 1);
        }
        let next = db.get("lost").unwrap().retry.next_ms;
        drop(db);
        db = root.open();
        assert!(db.claim("bob", "other", next - 1, 10).unwrap().is_none());
        assert!(matches!(
            db.acknowledge(&delivery.attempt, "s", next),
            Err(MailError::StaleAttempt)
        ));
        now = next;
    }
    assert!(db.claim("bob", "s", now, 10).unwrap().is_none());
    assert_eq!(db.unread("bob").unwrap(), 1);
    assert_eq!(
        db.get("lost").unwrap().retry.attempts,
        MAX_DELIVERY_ATTEMPTS
    );
}
#[test]
fn prior_schema_is_rejected_without_deleting_its_mail() {
    use std::os::unix::fs::PermissionsExt;
    let root = Root::new();
    let path = root.0.join("mail.sqlite3");
    let raw = rusqlite::Connection::open(&path).unwrap();
    // Original local version 1 table, not an external fixture.
    raw.execute_batch("CREATE TABLE mail(id TEXT PRIMARY KEY NOT NULL,sender TEXT NOT NULL,recipient TEXT NOT NULL,body BLOB NOT NULL,state INTEGER NOT NULL CHECK(state BETWEEN 0 AND 3),attempt INTEGER,session TEXT,expires INTEGER,CHECK((attempt IS NULL AND session IS NULL AND expires IS NULL) OR (state=0 AND attempt IS NOT NULL AND session IS NOT NULL AND expires IS NOT NULL))); CREATE TABLE sequence(id INTEGER PRIMARY KEY CHECK(id=1),value INTEGER NOT NULL CHECK(value>=0)); INSERT INTO sequence VALUES(1,7); PRAGMA application_id=1179468108; PRAGMA user_version=1;").unwrap();
    raw.execute(
        "INSERT INTO mail(id,sender,recipient,body,state) VALUES('old','alice','bob',?1,0)",
        [b"retained".as_slice()],
    )
    .unwrap();
    drop(raw);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(MailStore::open(&root.0), Err(MailError::Schema)));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let raw = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        raw.query_row("SELECT body FROM mail WHERE id='old'", [], |r| r
            .get::<_, Vec<u8>>(0))
            .unwrap(),
        b"retained"
    );
    assert_eq!(
        raw.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
