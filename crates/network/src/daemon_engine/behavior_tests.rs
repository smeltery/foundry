use super::*;
#[test]
fn credential_connection_and_conversation_all_required() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let alice = register(&mut engine, 1, "alice", &root.0);
    let bob = register(&mut engine, 2, "bob", &root.0);
    let forged = Auth {
        conversation: bob.conversation.clone(),
        credential: alice.credential,
    };
    assert!(matches!(
        handle(
            &mut engine,
            1,
            Request::Attach {
                auth: forged.clone()
            }
        ),
        ReplyResult::Error(EngineError::Unauthorized)
    ));
    assert!(matches!(
        handle(&mut engine, 1, send(&forged, "bad", "alice", "x")),
        ReplyResult::Error(EngineError::Unauthorized)
    ));
    assert!(matches!(
        handle(&mut engine, 3, send(&alice, "bad", "bob", "x")),
        ReplyResult::Error(EngineError::Unauthorized)
    ));
    ok(&mut engine, 1, send(&alice, "m", "bob", "hello"));
    let id = inbox(&mut engine, 2, &bob, true)
        .unwrap()
        .attempt_id
        .unwrap();
    ok(&mut engine, 3, Request::Attach { auth: bob.clone() });
    assert!(matches!(
        handle(
            &mut engine,
            3,
            Request::Delivered {
                auth: bob.clone(),
                attempt_id: id
            }
        ),
        ReplyResult::Error(EngineError::StaleAttempt)
    ));
    ok(
        &mut engine,
        2,
        Request::Attach {
            auth: alice.clone(),
        },
    );
    assert!(matches!(
        handle(
            &mut engine,
            2,
            Request::Delivered {
                auth: alice.clone(),
                attempt_id: id
            }
        ),
        ReplyResult::Error(EngineError::Unauthorized)
    ));
    assert!(matches!(
        handle(
            &mut engine,
            2,
            Request::PushFailed {
                auth: forged,
                attempt_id: id
            }
        ),
        ReplyResult::Error(EngineError::Unauthorized)
    ));
    assert_eq!(unread(&mut engine, 2, &bob), 1);
    ok(
        &mut engine,
        2,
        Request::Delivered {
            auth: bob,
            attempt_id: id,
        },
    );
}
#[test]
fn restart_preserves_registry_mail_and_clears_connection_authority() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let alice = register(&mut engine, 1, "alice", &root.0);
    let bob = register(&mut engine, 2, "bob", &root.0);
    ok(
        &mut engine,
        2,
        Request::SetStatus {
            auth: bob.clone(),
            status: "ready".into(),
            doing: "work".into(),
        },
    );
    ok(&mut engine, 1, send(&alice, "pending", "bob", "p"));
    let old = inbox(&mut engine, 2, &bob, true)
        .unwrap()
        .attempt_id
        .unwrap();
    drop(engine);
    let mut engine = open(&root, &time);
    assert!(matches!(
        handle(&mut engine, 2, Request::Unread { auth: bob.clone() }),
        ReplyResult::Error(EngineError::Unauthorized)
    ));
    match ok(
        &mut engine,
        9,
        Request::Who {
            filter: Filter::default(),
        },
    ) {
        Outcome::Directory { entries } => {
            assert_eq!(entries.len(), 2);
            assert!(entries.iter().all(|e| !e.online && e.repository.is_none()));
            assert!(
                entries
                    .iter()
                    .any(|e| e.status == "ready" && e.doing == "work")
            );
        }
        _ => panic!("unexpected directory"),
    }
    ok(&mut engine, 3, Request::Attach { auth: bob.clone() });
    assert_eq!(unread(&mut engine, 3, &bob), 1);
    assert!(matches!(
        handle(
            &mut engine,
            3,
            Request::Delivered {
                auth: bob.clone(),
                attempt_id: old
            }
        ),
        ReplyResult::Error(EngineError::StaleAttempt)
    ));
    time.store(31_000, Ordering::SeqCst);
    let new = inbox(&mut engine, 3, &bob, true)
        .unwrap()
        .attempt_id
        .unwrap();
    assert_ne!(old, new);
    ok(
        &mut engine,
        3,
        Request::Delivered {
            auth: bob.clone(),
            attempt_id: new,
        },
    );
    drop(engine);
    assert_eq!(
        MailStore::open(&root.0)
            .unwrap()
            .get("pending")
            .unwrap()
            .state,
        State::Delivered
    );
}
#[test]
fn disconnect_releases_only_its_scope_and_peek_failure_keeps_pending() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let alice = register(&mut engine, 1, "alice", &root.0);
    let bob = register(&mut engine, 2, "bob", &root.0);
    ok(&mut engine, 3, Request::Attach { auth: bob.clone() });
    ok(&mut engine, 1, send(&alice, "one", "bob", "1"));
    ok(&mut engine, 1, send(&alice, "two", "bob", "2"));
    let first_batch = match ok(
        &mut engine,
        2,
        Request::Inbox {
            auth: bob.clone(),
            peek: true,
            wait_secs: 0,
        },
    ) {
        Outcome::Inbox { messages, .. } => messages,
        _ => panic!("missing batch"),
    };
    let second_batch = match ok(
        &mut engine,
        3,
        Request::Inbox {
            auth: bob.clone(),
            peek: true,
            wait_secs: 0,
        },
    ) {
        Outcome::Inbox { messages, .. } => messages,
        _ => panic!("missing batch"),
    };
    assert_eq!(first_batch.len(), 2);
    assert_eq!(second_batch.len(), 2);
    let one = first_batch[0].attempt_id.unwrap();
    let same_mail = second_batch[0].attempt_id.unwrap();
    assert_ne!(one, same_mail);
    engine.disconnect(ConnectionId(2)).unwrap();
    ok(
        &mut engine,
        3,
        Request::Delivered {
            auth: bob.clone(),
            attempt_id: same_mail,
        },
    );
    assert_eq!(unread(&mut engine, 3, &bob), 1);
    ok(&mut engine, 4, Request::Attach { auth: bob.clone() });
    let retry = inbox(&mut engine, 4, &bob, true)
        .unwrap()
        .attempt_id
        .unwrap();
    ok(
        &mut engine,
        4,
        Request::PushFailed {
            auth: bob.clone(),
            attempt_id: retry,
        },
    );
    let final_id = inbox(&mut engine, 4, &bob, true)
        .unwrap()
        .attempt_id
        .unwrap();
    assert_ne!(retry, final_id);
    ok(
        &mut engine,
        4,
        Request::Delivered {
            auth: bob.clone(),
            attempt_id: final_id,
        },
    );
    assert_eq!(unread(&mut engine, 4, &bob), 0);
}
#[test]
fn trusted_registration_pull_capability_linking_and_exact_filters() {
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let alice = register(&mut engine, 1, "alice", &root.0);
    assert_eq!(alice.conversation.person, "person");
    assert_eq!(alice.conversation.machine, "machine");
    register(&mut engine, 1, "bob", &root.0);
    assert!(matches!(
        handle(&mut engine, 1, Request::Linked {}),
        ReplyResult::Error(EngineError::Ambiguous)
    ));
    let mut broad = send(&alice, "m", "bob", "x");
    if let Request::Send { to, .. } = &mut broad {
        *to = Filter::default();
    }
    assert!(matches!(
        handle(&mut engine, 1, broad),
        ReplyResult::Error(EngineError::Ambiguous)
    ));
    // Build exact filters through JSON, also exercising the real request decoder.
    let mut value = serde_json::to_value(send(&alice, "m", "bob", "x")).unwrap();
    value["to"]["person"] = "absent".into();
    assert!(matches!(
        handle(&mut engine, 1, serde_json::from_value(value).unwrap()),
        ReplyResult::Error(EngineError::NotFound)
    ));
    match ok(
        &mut engine,
        1,
        Request::Who {
            filter: Filter::default(),
        },
    ) {
        Outcome::Directory { entries } => assert!(
            entries
                .iter()
                .all(|e| matches!(e.capabilities.as_slice(), [Capability::Pull]))
        ),
        _ => panic!("unexpected directory"),
    }
    let other = Conversation {
        person: "ab".into(),
        machine: "c".into(),
        name: "d".into(),
    };
    let another = Conversation {
        person: "a".into(),
        machine: "bc".into(),
        name: "d".into(),
    };
    assert_ne!(mailbox_address(&other), mailbox_address(&another));
    assert!(mailbox_address(&other).len() <= 256);
}
#[test]
fn shutdown_and_storage_uncertainty_are_explicit() {
    use std::os::unix::fs::PermissionsExt;
    let root = Root::new();
    let time = Arc::new(AtomicI64::new(0));
    let mut engine = open(&root, &time);
    let auth = register(&mut engine, 1, "a", &root.0);
    assert!(
        !engine
            .handle(
                ConnectionId(2),
                RequestEnvelope {
                    id: 1,
                    request: Request::Shutdown { auth: auth.clone() }
                }
            )
            .should_shutdown()
    );
    assert!(
        engine
            .handle(
                ConnectionId(1),
                RequestEnvelope {
                    id: 2,
                    request: Request::Shutdown { auth: auth.clone() }
                }
            )
            .should_shutdown()
    );
    std::fs::set_permissions(
        root.0.join("mail.sqlite3"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(matches!(
        handle(&mut engine, 1, send(&auth, "m", "a", "text")),
        ReplyResult::Error(EngineError::StorageUncertain)
    ));
    assert!(matches!(
        handle(
            &mut engine,
            1,
            Request::Who {
                filter: Filter::default()
            }
        ),
        ReplyResult::Error(EngineError::StorageUncertain)
    ));
}
