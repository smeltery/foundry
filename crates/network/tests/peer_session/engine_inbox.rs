use super::*;
use cap_std::{ambient_authority, fs::Dir};
use foundry_network::{
    daemon_engine::{
        self, Engine, Outcome, ReplyResult, Request, RequestEnvelope, TrustedIdentity,
    },
    session_registry::FolderPolicy,
};
use std::sync::Mutex;

#[tokio::test]
async fn real_peer_receipt_renders_text_and_metadata_through_engine_inbox() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let directory = Dir::open_ambient_dir(&b.root, ambient_authority()).unwrap();
    let ownership = daemon_lease::DaemonLease::acquire(&directory).unwrap();
    let engine = Arc::new(Mutex::new(
        Engine::open_with_clock(
            &b.root,
            FolderPolicy::default(),
            TrustedIdentity {
                person: "bob".into(),
                machine: "desktop".into(),
            },
            || Ok(NOW + 100),
        )
        .unwrap(),
    ));
    let connection = daemon_engine::ConnectionId(1);
    let auth = match engine
        .lock()
        .unwrap()
        .handle(
            connection,
            RequestEnvelope {
                id: 1,
                request: Request::Register {
                    name: "terminal".into(),
                    repository: b.root.clone(),
                    share_path: false,
                },
            },
        )
        .result
    {
        ReplyResult::Ok(Outcome::Registered { auth }) => auth,
        _ => panic!("registration failed"),
    };
    let destination = daemon_engine::mailbox_address(&auth.conversation);
    let resolve_engine = engine.clone();
    let mailbox = TextMailbox::new(
        mail_store::MailStore::open(&b.root).unwrap(),
        move |address| {
            if address != "bob/desktop/terminal" {
                return Err(PeerError::Unauthorized);
            }
            let response = resolve_engine.lock().unwrap().handle(
                daemon_engine::ConnectionId(2),
                RequestEnvelope {
                    id: 2,
                    request: Request::Who {
                        filter: daemon_engine::Filter {
                            person: Some("bob".into()),
                            machine: Some("desktop".into()),
                            name: Some("terminal".into()),
                            ..Default::default()
                        },
                    },
                },
            );
            match response.result {
                ReplyResult::Ok(Outcome::Directory { entries }) if entries.len() == 1 => {
                    Ok(destination.clone())
                }
                _ => Err(PeerError::Unauthorized),
            }
        },
    );
    let authority = AdmittedTeam::new(
        b.store(),
        ownership,
        b.identity.endpoint_id(),
        || Ok(NOW + 100),
        Some(mailbox),
    )
    .unwrap();
    let (left, right) = (a.endpoint().await, b.endpoint().await);
    let (cancel, _) = watch::channel(false);
    let (mut sender, mut receiver) =
        pair(&left, &right, a.authority(false), authority, &cancel).await;
    let mut original = message();
    // A JSON-looking user body is still literal text; no envelope heuristics.
    original.body = "{\"literal\":\"user text\"}".into();
    original.notify = true;
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
    let result = engine.lock().unwrap().handle(
        connection,
        RequestEnvelope {
            id: 3,
            request: Request::Inbox {
                auth: auth.clone(),
                wait_secs: 0,
                peek: true,
            },
        },
    );
    match result.result {
        ReplyResult::Ok(Outcome::Inbox { messages, text }) => {
            assert_eq!(messages.len(), 1);
            assert!(!text.is_empty());
            let mail = messages.into_iter().next().unwrap();
            let bytes = data_encoding::BASE64
                .decode(mail.body_base64.as_bytes())
                .unwrap();
            assert_eq!(bytes, original.body.as_bytes());
            assert_eq!(mail.reply_to, original.reply_to);
            assert_eq!(mail.message_id, original.id);
            assert_eq!((mail.wake, mail.notify), (original.wake, original.notify));
        }
        _ => panic!("expected peer text in local inbox"),
    }
    let persisted = mail_store::MailStore::open(&b.root)
        .unwrap()
        .get(&original.id)
        .unwrap();
    assert_eq!(
        persisted.message.encoding,
        mail_store::PayloadEncoding::WireJson
    );
    assert_eq!(
        serde_json::from_slice::<wire::Message>(&persisted.message.body).unwrap(),
        original
    );
    drop((sender, receiver, engine));
    tokio::join!(left.shutdown(), right.shutdown());
}
