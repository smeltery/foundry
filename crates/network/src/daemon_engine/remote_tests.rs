use super::*;
use foundry_network::{mail_store::TransportDisposition, wire};
use iroh::{EndpointId, SecretKey};
fn peer(n: u8) -> EndpointId {
    SecretKey::from_bytes(&[n; 32]).public()
}
fn member(n: u8, person: &str, machine: &str) -> RemoteMember {
    RemoteMember {
        peer: peer(n),
        person: person.into(),
        machine: machine.into(),
    }
}
fn presence(person: &str, machine: &str, name: &str) -> wire::Presence {
    wire::Presence {
        sessions: vec![wire::SessionPresence {
            address: format!("{person}/{machine}/{name}"),
            person: person.into(),
            machine: machine.into(),
            name: name.into(),
            agent: "client".into(),
            cwd: String::new(),
            repo: None,
            branch: None,
            status: "ready".into(),
            since: None,
            doing: None,
            tier: wire::DeliveryTier::Pull,
            unread: 0,
            is_me: true,
        }],
        ..Default::default()
    }
}
#[test]
fn remote_send_authorization_receipt_and_reopen_keep_wire_and_local_mail_separate() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(71));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "sender", &root.0);
    let remote = member(1, "bob", "remote");
    engine
        .sync_remote_members("team", 2, std::slice::from_ref(&remote))
        .unwrap();
    engine
        .remote_presence(remote.peer, 5, &presence("bob", "remote", "reader"))
        .unwrap();
    let request = send(&auth, "wire-id", "reader", "body");
    assert!(matches!(
        handle(&mut engine, 2, request),
        ReplyResult::Error(EngineError::Unauthorized)
    ));
    assert!(engine.authorize_control(ConnectionId(2), &auth).is_err());
    engine.authorize_control(ConnectionId(1), &auth).unwrap();
    let queued = ok(&mut engine, 1, send(&auth, "wire-id", "reader", "body"));
    assert!(matches!(queued,Outcome::TransportQueued {message_id,..} if message_id=="wire-id"));
    let mut store = MailStore::open(&root.0).unwrap();
    let first = store
        .transport_for("team", remote.peer, 71)
        .unwrap()
        .remove(0);
    assert_eq!(first.message.from, "person/machine/sender");
    assert_eq!(first.message.to, "bob/remote/reader");
    assert_eq!(first.message.sent_at, 71);
    assert!(first.message.from_repo.is_none());
    let attempt = store.begin_transport_attempt(&first, 5).unwrap();
    time.store(72, Ordering::SeqCst);
    ok(
        &mut engine,
        1,
        send(&auth, "wire-id", "reader", "replacement"),
    );
    assert!(
        store
            .finish_transport_attempt(&attempt, remote.peer, 5, TransportDisposition::Queued)
            .is_err()
    );
    assert_eq!(
        store.unread(&mailbox_address(&auth.conversation)).unwrap(),
        0
    );
    drop(engine);
    let mut engine = open(&root, &time);
    ok(&mut engine, 3, Request::Attach { auth: auth.clone() });
    assert!(matches!(
        handle(&mut engine, 3, send(&auth, "next", "reader", "body")),
        ReplyResult::Error(EngineError::NotFound)
    ));
    engine
        .sync_remote_members("team", 2, std::slice::from_ref(&remote))
        .unwrap();
    // Offline routing is retained, but never claims the disconnected peer is online.
    match ok(
        &mut engine,
        3,
        Request::Who {
            filter: Filter {
                name: Some("reader".into()),
                ..Default::default()
            },
        },
    ) {
        Outcome::Directory { entries } => assert!(!entries[0].online),
        _ => panic!("directory"),
    }
    ok(
        &mut engine,
        3,
        send(&auth, "offline-queued", "reader", "pending"),
    );
    engine
        .remote_presence(remote.peer, 0, &presence("bob", "remote", "reader"))
        .unwrap();
    let replay = store.transport_for("team", remote.peer, 72).unwrap();
    assert_eq!(replay[0].message.sent_at, 72);
    assert_eq!(replay[0].message.body, "replacement");
    engine.suspend_remote_directory();
    assert!(matches!(
        handle(&mut engine, 3, send(&auth, "blocked", "reader", "body")),
        ReplyResult::Error(EngineError::NotFound)
    ));
}
#[test]
fn authenticated_namespace_revocation_ambiguity_and_connection_generations() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(1));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "sender", &root.0);
    let one = member(2, "bob", "one");
    let two = member(3, "bob", "two");
    engine
        .sync_remote_members("team", 1, &[one.clone(), two.clone()])
        .unwrap();
    let forged = presence("forged", "forged", "reader");
    engine.remote_presence(one.peer, 10, &forged).unwrap();
    engine
        .remote_presence(two.peer, 20, &presence("bob", "two", "reader"))
        .unwrap();
    assert!(matches!(
        handle(&mut engine, 1, send(&auth, "ambiguous", "reader", "body")),
        ReplyResult::Error(EngineError::Ambiguous)
    ));
    match ok(
        &mut engine,
        1,
        Request::Who {
            filter: Filter {
                machine: Some("one".into()),
                ..Default::default()
            },
        },
    ) {
        Outcome::Directory { entries } => {
            assert_eq!(entries[0].conversation.person, "bob");
            assert_eq!(entries[0].conversation.machine, "one");
        }
        _ => panic!("directory"),
    }
    engine
        .remote_presence(one.peer, 11, &presence("bob", "one", "reader"))
        .unwrap();
    engine.remote_disconnected(one.peer, 10).unwrap();
    assert!(engine.remote_presence(one.peer, 10, &forged).is_err());
    engine.remote_disconnected(one.peer, 11).unwrap();
    assert!(engine.remote_presence(one.peer, 11, &forged).is_err());
    engine
        .sync_remote_members("team", 2, std::slice::from_ref(&one))
        .unwrap();
    assert!(
        engine
            .remote_presence(two.peer, 21, &presence("bob", "two", "reader"))
            .is_err()
    );
    assert!(
        engine
            .sync_remote_members("team", 1, std::slice::from_ref(&one))
            .is_err()
    );
    engine.sync_remote_members("team", 3, &[]).unwrap();
    assert!(matches!(
        handle(&mut engine, 1, send(&auth, "revoked", "reader", "body")),
        ReplyResult::Error(EngineError::NotFound)
    ));
}
#[test]
fn incoming_current_recipient_folder_and_presence_share_visibility() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "reader", &root.0);
    assert_eq!(
        engine
            .resolve_incoming_recipient("person/machine/reader")
            .unwrap(),
        mailbox_address(&auth.conversation)
    );
    assert!(
        engine
            .resolve_incoming_recipient("other/machine/reader")
            .is_err()
    );
    let published = engine.local_presence().unwrap();
    assert_eq!(published.sessions[0].address, "person/machine/reader");
    assert!(published.sessions[0].repo.is_none());
    assert_eq!(published.sessions[0].cwd, "");
    engine.disconnect(ConnectionId(1)).unwrap();
    assert!(engine.local_presence().unwrap().sessions.is_empty());
    assert_eq!(
        engine
            .resolve_incoming_recipient("person/machine/reader")
            .unwrap(),
        mailbox_address(&auth.conversation)
    );
    drop(engine);
    let policy = FolderPolicy::new(vec![(root.0.clone(), false)]).unwrap();
    let engine = Engine::open(
        &root.0,
        policy,
        TrustedIdentity {
            person: "person".into(),
            machine: "machine".into(),
        },
    )
    .unwrap();
    assert!(matches!(
        engine.resolve_incoming_recipient("person/machine/reader"),
        Err(EngineError::NotFound)
    ));
    assert!(engine.local_presence().unwrap().sessions.is_empty());
}
#[test]
fn offline_known_routes_and_remote_repository_filters_are_literal_and_scoped() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "sender", &root.0);
    let remote = member(4, "bob", "remote");
    engine
        .sync_remote_members("team", 1, std::slice::from_ref(&remote))
        .unwrap();
    let mut view = presence("bob", "remote", "live");
    view.sessions[0].repo = Some("/remote/only/project".into());
    view.known.push("bob/remote/offline".into());
    engine.remote_presence(remote.peer, 1, &view).unwrap();
    let filter = Filter {
        repository: Some("/remote/only/project".into()),
        ..Default::default()
    };
    match ok(
        &mut engine,
        1,
        Request::Who {
            filter: filter.clone(),
        },
    ) {
        Outcome::Directory { entries } => assert_eq!(entries[0].conversation.name, "live"),
        _ => panic!("directory"),
    }
    let Request::Send {
        auth,
        message_id,
        body,
        reply_to,
        wake,
        notify,
        ..
    } = send(&auth, "repo", "live", "body")
    else {
        panic!("send")
    };
    assert!(matches!(
        ok(
            &mut engine,
            1,
            Request::Send {
                auth,
                message_id,
                body,
                reply_to,
                wake,
                notify,
                to: filter
            }
        ),
        Outcome::TransportQueued { .. }
    ));
}

#[test]
fn exact_roster_sync_is_read_only_under_write_lock_and_changes_persist() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let one = member(5, "bob", "one");
    let two = member(6, "bob", "two");
    let third = member(7, "chris", "three");
    let original = [one.clone(), two.clone()];
    engine.sync_remote_members("team", 1, &original).unwrap();
    engine
        .remote_presence(one.peer, 1, &presence("bob", "one", "reader"))
        .unwrap();
    engine.remote_disconnected(third.peer, 77).unwrap();
    engine.suspend_remote_directory();
    let raw = rusqlite::Connection::open(root.0.join("mail.sqlite3")).unwrap();
    raw.execute_batch("BEGIN IMMEDIATE").unwrap();
    // Another connection owns the writer lock: success requires a read-only
    // comparison, not a timed retry or a deferred write after this scope ends.
    let reversed = [two.clone(), one.clone()];
    engine.sync_remote_members("team", 1, &reversed).unwrap();
    engine.sync_remote_members("team", 1, &original).unwrap();
    assert!(matches!(
        engine.sync_remote_members("team", 0, &original),
        Err(EngineError::StaleAttempt)
    ));
    assert!(matches!(
        engine.sync_remote_members("team", 1, std::slice::from_ref(&one)),
        Err(EngineError::Conflict)
    ));
    match ok(
        &mut engine,
        1,
        Request::Who {
            filter: Filter {
                person: Some("bob".into()),
                ..Default::default()
            },
        },
    ) {
        Outcome::Directory { entries } => assert_eq!(entries.len(), 1),
        _ => panic!("directory"),
    }
    let revision: Vec<u8> = raw
        .query_row("SELECT revision FROM remote_epoch WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(revision, 1_u64.to_be_bytes());
    raw.execute_batch("ROLLBACK").unwrap();
    let changed = [one.clone(), third.clone()];
    engine.sync_remote_members("team", 2, &changed).unwrap();
    // The no-op still pruned the retired generation for a nonmember. Adding it
    // later must not resurrect that unrelated connection's generation fence.
    engine
        .remote_presence(third.peer, 0, &presence("chris", "three", "reader"))
        .unwrap();
    let revision: Vec<u8> = raw
        .query_row("SELECT revision FROM remote_epoch WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(revision, 2_u64.to_be_bytes());
    assert_eq!(
        raw.query_row(
            "SELECT count(*) FROM remote_members WHERE peer=?1",
            [third.peer.as_bytes()],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        raw.query_row(
            "SELECT count(*) FROM remote_members WHERE peer=?1",
            [two.peer.as_bytes()],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    drop(engine);
    let mut reopened = open(&root, &time);
    assert!(matches!(
        reopened.sync_remote_members("team", 1, &original),
        Err(EngineError::StaleAttempt)
    ));
    reopened
        .sync_remote_members("next-team", 2, &changed)
        .unwrap();
    assert_eq!(
        raw.query_row("SELECT team FROM remote_epoch WHERE id=1", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "next-team"
    );
}

#[path = "remote_bounds_tests.rs"]
mod remote_bounds_tests;
