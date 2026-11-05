//! Equal-time mail keeps arrival order even when identifiers sort the other way.
#![cfg(unix)]
use foundry_network::{
    mail_store::{DeliveryIntent, MailStore, Message, PayloadEncoding, Settlement, State},
    wire,
};
use std::path::PathBuf;

/// Arrival order is the reverse of lexical order, so an id sort would be caught.
const ARRIVALS: [&str; 4] = ["ZZ-last-sorted", "QQ-mid", "MM-mid", "AA-first-sorted"];

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path =
            std::env::temp_dir().join(format!("arrival-order-{:032x}", u128::from_le_bytes(nonce)));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        // Report, never panic: a second panic would hide the test's own failure.
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("cleanup of {:?} failed: {error}", self.0);
        }
    }
}
fn local(id: &str, recipient: &str) -> Message {
    Message {
        id: id.into(),
        sender: "writer".into(),
        recipient: recipient.into(),
        body: id.as_bytes().to_vec(),
        encoding: PayloadEncoding::Text,
        reply_to: None,
        intent: DeliveryIntent::default(),
    }
}
fn remote(id: &str, sent_at: i64) -> wire::Message {
    wire::Message {
        id: id.into(),
        from: "amber/desk/writer".into(),
        from_agent: None,
        from_repo: None,
        to: "birch/desk/reader".into(),
        via: None,
        reply_to: None,
        body: id.into(),
        wake: true,
        notify: false,
        sent_at,
        approval: None,
        files: Vec::new(),
    }
}
fn ids<'a>(values: impl IntoIterator<Item = &'a Message>) -> Vec<&'a str> {
    values.into_iter().map(|m| m.id.as_str()).collect()
}

#[test]
fn unread_mail_is_listed_and_claimed_in_arrival_order() {
    let root = Root::new();
    let mut store = MailStore::open(&root.0).unwrap();
    for id in ARRIVALS {
        store.insert(&local(id, "reader"), State::Pending).unwrap();
    }
    assert_eq!(ids(&store.peek("reader", 10).unwrap()), ARRIVALS);
    let polled = store.poll_batch("reader", "scope-a", 50, true).unwrap();
    assert_eq!(ids(polled.iter().map(|r| &r.message)), ARRIVALS);
    // Push claims drain one at a time; a fresh store must keep the same order.
    drop(store);
    let mut store = MailStore::open(&root.0).unwrap();
    store.disconnect("scope-a").unwrap();
    let mut claimed = Vec::new();
    while let Some(delivery) = store.claim("reader", "scope-b", 50, 1_000).unwrap() {
        claimed.push(delivery.mail.id);
    }
    assert_eq!(claimed, ARRIVALS);
}

#[test]
fn held_mail_released_out_of_order_still_reads_in_arrival_order() {
    let root = Root::new();
    let mut store = MailStore::open(&root.0).unwrap();
    for id in ARRIVALS {
        store.insert(&local(id, "reader"), State::Held).unwrap();
    }
    let held: Vec<_> = store.outbox("writer", 10).unwrap();
    assert!(held.iter().all(|m| m.state == State::Held));
    assert_eq!(ids(held.iter().map(|m| &m.message)), ARRIVALS);
    assert!(store.peek("reader", 10).unwrap().is_empty());
    // Approval order is unrelated to arrival order and must not reorder reads.
    for id in ARRIVALS.iter().rev() {
        store.settle(id, Settlement::Release).unwrap();
    }
    assert_eq!(ids(&store.peek("reader", 10).unwrap()), ARRIVALS);
}

#[test]
fn outbox_history_keeps_arrival_order_across_recipients_and_states() {
    let root = Root::new();
    let mut store = MailStore::open(&root.0).unwrap();
    for (index, id) in ARRIVALS.iter().enumerate() {
        let recipient = if index % 2 == 0 { "reader" } else { "other" };
        let state = if index == 1 {
            State::Held
        } else {
            State::Pending
        };
        store.insert(&local(id, recipient), state).unwrap();
    }
    let history = store.outbox("writer", 10).unwrap();
    assert_eq!(ids(history.iter().map(|m| &m.message)), ARRIVALS);
}

#[test]
fn equal_timestamp_transport_replays_in_enqueue_order() {
    let root = Root::new();
    let mut store = MailStore::open(&root.0).unwrap();
    let peer = iroh::SecretKey::from_bytes(&[7; 32]).public();
    for id in ARRIVALS {
        store
            .enqueue_transport("team", peer, &remote(id, 900))
            .unwrap();
    }
    let replay: Vec<_> = store
        .transport_for("team", peer, 900)
        .unwrap()
        .into_iter()
        .map(|queued| queued.message.id)
        .collect();
    assert_eq!(replay, ARRIVALS);
    drop(store);
    let mut store = MailStore::open(&root.0).unwrap();
    let reopened: Vec<_> = store
        .transport_for("team", peer, 900)
        .unwrap()
        .into_iter()
        .map(|queued| queued.message.id)
        .collect();
    assert_eq!(reopened, ARRIVALS);
}
