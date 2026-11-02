#![cfg(unix)]
use foundry_network::peer_session::*;
pub use foundry_network::{
    Channel, NetworkEndpoint, NetworkError, PeerFrame, WireProfile, daemon_lease, full_team_store,
    mail_store, read_frame, signing, team, wire, write_frame,
};
use foundry_network::{
    daemon_engine::{
        Auth, ConnectionId, Engine, Outcome, ReplyResult, Request, RequestEnvelope,
        TrustedIdentity, mailbox_address,
    },
    mail_store::{MailStore, PayloadEncoding, State},
    session_registry::FolderPolicy,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
#[allow(dead_code)]
#[path = "peer_session/helpers.rs"]
mod helpers;
use helpers::*;
fn handle(engine: &mut Engine, connection: u64, request: Request) -> Outcome {
    match engine
        .handle(ConnectionId(connection), RequestEnvelope { id: 1, request })
        .result
    {
        ReplyResult::Ok(value) => value,
        ReplyResult::Error(error) => panic!("{error}"),
    }
}
fn open(node: &Node) -> Engine {
    Engine::open(
        &node.root,
        FolderPolicy::default(),
        TrustedIdentity {
            person: "bob".into(),
            machine: "desktop".into(),
        },
    )
    .unwrap()
}
fn register(engine: &mut Engine, node: &Node) -> Auth {
    match handle(
        engine,
        1,
        Request::Register {
            name: "terminal".into(),
            repository: node.root.clone(),
            share_path: false,
        },
    ) {
        Outcome::Registered { auth } => auth,
        _ => panic!("missing registration"),
    }
}
#[tokio::test]
async fn real_peer_receipt_displays_text_retains_envelope_and_rejects_conflicts() {
    use cap_std::{ambient_authority, fs::Dir};
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let directory = Dir::open_ambient_dir(&b.root, ambient_authority()).unwrap();
    let owner = daemon_lease::DaemonLease::acquire(&directory).unwrap();
    let mut engine = open(&b);
    let auth = register(&mut engine, &b);
    let address = mailbox_address(&auth.conversation);
    let expected = address.clone();
    let mailbox = TextMailbox::new(MailStore::open(&b.root).unwrap(), move |to| {
        if to == "bob/desktop/terminal" {
            Ok(expected.clone())
        } else {
            Err(PeerError::Unauthorized)
        }
    });
    let authority = AdmittedTeam::new(
        b.store(),
        owner,
        b.identity.endpoint_id(),
        || Ok(NOW + 100),
        Some(mailbox),
    )
    .unwrap();
    let left_authority = a.authority(false);
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (mut sender, mut receiver) = pair(
        &left,
        &right,
        left_authority.clone(),
        authority.clone(),
        &cancel,
    )
    .await;
    let original = message();
    sender.send_text(original.clone()).await.unwrap();
    assert!(matches!(
        receiver.receive().await.unwrap(),
        PeerEvent::Received {
            duplicate: false,
            ..
        }
    ));
    assert!(matches!(
        sender.receive().await.unwrap(),
        PeerEvent::Queued { .. }
    ));
    match handle(
        &mut engine,
        1,
        Request::Inbox {
            auth: auth.clone(),
            wait_secs: 0,
            peek: true,
        },
    ) {
        Outcome::Inbox { messages, .. } => {
            assert_eq!(messages.len(), 1);
            let mail = messages.into_iter().next().unwrap();
            assert_eq!(
                data_encoding::BASE64
                    .decode(mail.body_base64.as_bytes())
                    .unwrap(),
                original.body.as_bytes()
            );
            assert_eq!(mail.reply_to, original.reply_to);
            assert!(mail.attempt_id.is_some());
        }
        _ => panic!("missing peer inbox text"),
    }
    let saved = MailStore::open(&b.root).unwrap().get(&original.id).unwrap();
    assert_eq!(saved.message.encoding, PayloadEncoding::WireJson);
    assert_eq!(saved.message.body, serde_json::to_vec(&original).unwrap());
    assert_eq!(saved.state, State::Pending);
    sender.send_text(original.clone()).await.unwrap();
    assert!(matches!(
        receiver.receive().await.unwrap(),
        PeerEvent::Received {
            duplicate: true,
            ..
        }
    ));
    sender.receive().await.unwrap();
    drop((sender, receiver));
    for changed in [
        wire::Message {
            sent_at: original.sent_at + 1,
            ..original.clone()
        },
        wire::Message {
            notify: true,
            ..original.clone()
        },
    ] {
        let (mut sender, mut receiver) = pair(
            &left,
            &right,
            left_authority.clone(),
            authority.clone(),
            &cancel,
        )
        .await;
        sender.send_text(changed).await.unwrap();
        let note = match receiver.receive().await.unwrap() {
            PeerEvent::Rejected { message_id, note } => {
                assert_eq!(message_id, original.id);
                assert_eq!(note, PeerError::Invalid.to_string());
                note
            }
            _ => panic!("expected conflicting envelope refusal"),
        };
        assert!(!note.is_empty());
        assert!(matches!(
            sender.receive().await.unwrap(),
            PeerEvent::Receipt { message_id, disposition: ReceiptDisposition::Refused, note: actual }
                if message_id == original.id && actual == note
        ));
        let unchanged = MailStore::open(&b.root).unwrap().get(&original.id).unwrap();
        assert_eq!(unchanged.message, saved.message);
        assert_eq!(unchanged.state, saved.state);
        assert_eq!(unchanged.retry, saved.retry);
        drop((sender, receiver));
    }
    drop(engine);
    let mut engine = open(&b);
    handle(&mut engine, 2, Request::Attach { auth: auth.clone() });
    let attempt = match handle(
        &mut engine,
        2,
        Request::Inbox {
            auth: auth.clone(),
            wait_secs: 0,
            peek: true,
        },
    ) {
        Outcome::Inbox { messages, .. } => {
            assert_eq!(messages.len(), 1);
            let mail = messages.into_iter().next().unwrap();
            assert_eq!(
                data_encoding::BASE64
                    .decode(mail.body_base64.as_bytes())
                    .unwrap(),
                original.body.as_bytes()
            );
            mail.attempt_id.unwrap()
        }
        _ => panic!("missing restarted peer inbox"),
    };
    handle(
        &mut engine,
        2,
        Request::Delivered {
            auth,
            attempt_id: attempt,
        },
    );
    let saved = MailStore::open(&b.root).unwrap().get(&original.id).unwrap();
    assert_eq!(saved.state, State::Delivered);
    assert_eq!(saved.message.body, serde_json::to_vec(&original).unwrap());
    drop((engine, authority, left_authority));
    tokio::join!(left.shutdown(), right.shutdown());
}
