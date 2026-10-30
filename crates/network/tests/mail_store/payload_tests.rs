use super::*;
use foundry_network::{mail_store::PayloadEncoding, wire};
fn envelope(id: &str) -> wire::Message {
    wire::Message {
        id: id.into(),
        from: "alice/desktop/sender".into(),
        from_agent: None,
        from_repo: None,
        to: "bob/desktop/receiver".into(),
        via: None,
        reply_to: Some("thread".into()),
        body: "visible body".into(),
        wake: true,
        notify: false,
        sent_at: 200,
        approval: None,
        files: Vec::new(),
    }
}
fn record(wire: &wire::Message) -> Message {
    Message {
        id: wire.id.clone(),
        sender: wire.from.clone(),
        recipient: "bob".into(),
        body: serde_json::to_vec(wire).unwrap(),
        encoding: PayloadEncoding::WireJson,
        reply_to: wire.reply_to.clone(),
        intent: DeliveryIntent {
            wake: wire.wake,
            notify: wire.notify,
        },
    }
}
#[test]
fn explicit_encoding_keeps_json_looking_local_text_literal() {
    let root = Root::new();
    let mut store = root.open();
    let text = br#"{"body":"inner text","sent_at":200}"#.to_vec();
    let local = Message {
        body: text.clone(),
        ..message("local")
    };
    assert_eq!(local.encoding, PayloadEncoding::default());
    store.insert(&local, State::Pending).unwrap();
    let read = store
        .poll_batch("bob", "s", 0, true)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(read.message.display_body().unwrap(), text);
    drop(store);
    assert_eq!(
        root.open().get("local").unwrap().message.encoding,
        PayloadEncoding::Text
    );
}
#[test]
fn immutable_full_envelope_and_encoding_survive_restart() {
    let root = Root::new();
    let mut store = root.open();
    let wire = envelope("wire");
    let original = record(&wire);
    store.insert(&original, State::Pending).unwrap();
    assert_eq!(
        store.insert(&original, State::Pending).unwrap(),
        Insert::Duplicate
    );
    for changed in [
        wire::Message {
            sent_at: wire.sent_at + 1,
            ..wire.clone()
        },
        wire::Message {
            from_repo: Some("retained metadata".into()),
            ..wire.clone()
        },
        wire::Message {
            notify: true,
            ..wire.clone()
        },
    ] {
        assert!(matches!(
            store.insert(&record(&changed), State::Pending),
            Err(MailError::Conflict)
        ));
    }
    assert!(matches!(
        store.insert(
            &Message {
                encoding: PayloadEncoding::Text,
                ..original.clone()
            },
            State::Pending
        ),
        Err(MailError::Conflict)
    ));
    drop(store);
    let mut store = root.open();
    let saved = store.get("wire").unwrap();
    assert_eq!(saved.message, original);
    let raw = rusqlite::Connection::open(root.0.join("mail.sqlite3")).unwrap();
    let bytes: usize = raw
        .query_row("SELECT sum(length(body)) FROM mail", [], |r| r.get(0))
        .unwrap();
    assert_eq!(bytes, original.body.len());
    drop(raw);
    assert_eq!(saved.message.body, serde_json::to_vec(&wire).unwrap());
    let read = store
        .poll_batch("bob", "s", 0, false)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(read.message.display_body().unwrap(), wire.body.as_bytes());
    assert_eq!(store.get("wire").unwrap().message.body, original.body);
}
#[test]
fn unsupported_or_corrupt_envelopes_do_not_consume_mail() {
    let root = Root::new();
    let mut store = root.open();
    let wire = envelope("guarded");
    let mut unsupported = envelope("files");
    unsupported.files.push(wire::TextAttachment {
        name: "file".into(),
        content: "unhandled".into(),
        path: None,
    });
    assert!(matches!(
        store.insert(&record(&unsupported), State::Pending),
        Err(MailError::UnsupportedPayload)
    ));
    unsupported = envelope("approval");
    unsupported.approval = Some(wire::SignedDocument {
        body: serde_json::json!({}),
        by: "unverified".into(),
        sig: "unverified".into(),
    });
    assert!(matches!(
        store.insert(&record(&unsupported), State::Pending),
        Err(MailError::UnsupportedPayload)
    ));
    store.insert(&record(&wire), State::Pending).unwrap();
    let raw = rusqlite::Connection::open(root.0.join("mail.sqlite3")).unwrap();
    raw.execute(
        "UPDATE mail SET body=?1 WHERE id='guarded'",
        [b"invalid envelope".as_slice()],
    )
    .unwrap();
    assert!(matches!(
        store.poll_batch("bob", "s", 0, false),
        Err(MailError::InvalidPayload)
    ));
    let mut unsupported = wire.clone();
    unsupported.files.push(wire::TextAttachment {
        name: "file".into(),
        content: "unhandled".into(),
        path: None,
    });
    raw.execute(
        "UPDATE mail SET body=?1 WHERE id='guarded'",
        [serde_json::to_vec(&unsupported).unwrap()],
    )
    .unwrap();
    assert!(matches!(
        store.poll_batch("bob", "s", 0, false),
        Err(MailError::UnsupportedPayload)
    ));
    assert_eq!(store.unread("bob").unwrap(), 1);
    assert_eq!(store.get("guarded").unwrap().state, State::Pending);
}
