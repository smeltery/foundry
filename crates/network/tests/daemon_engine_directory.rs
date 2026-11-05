//! Engine directory behavior: local terminal preference, published presence,
//! retained offline names and pull delivery.
#![cfg(unix)]
#[allow(dead_code)]
#[path = "daemon_engine_support/mod.rs"]
mod support;
use foundry_network::daemon_engine::*;
use support::*;

fn person_terminal(machine: Option<&str>) -> Filter {
    Filter {
        person: Some(PERSON.into()),
        machine: machine.map(Into::into),
        name: Some("terminal".into()),
        ..Filter::default()
    }
}
fn named(name: &str) -> Filter {
    Filter {
        name: Some(name.into()),
        ..Filter::default()
    }
}
fn unread(engine: &mut Engine, connection: u64, auth: &Auth) -> u64 {
    match ok(engine, connection, Request::Unread { auth: auth.clone() }) {
        Outcome::Unread { count } => count,
        _ => panic!("unexpected unread outcome"),
    }
}
fn peeked(engine: &mut Engine, connection: u64, auth: &Auth) -> Vec<Mail> {
    let request = Request::Inbox {
        auth: auth.clone(),
        wait_secs: 0,
        peek: true,
    };
    match ok(engine, connection, request) {
        Outcome::Inbox { messages, .. } => messages,
        _ => panic!("unexpected inbox outcome"),
    }
}

#[test]
fn unqualified_terminal_prefers_the_local_one_and_machine_selects_the_remote() {
    let fixture = Fixture::new();
    let mut engine = fixture.open();
    let local = register(&mut engine, 1, "terminal", &fixture.root);
    let desk = member(1, PERSON, "desk");
    engine
        .sync_remote_members("team", 1, std::slice::from_ref(&desk))
        .unwrap();
    engine
        .remote_presence(desk.peer, 1, &advertised("terminal", None))
        .unwrap();
    // Both terminals are live and listed; only routing picks one.
    assert_eq!(directory(&mut engine, 1, person_terminal(None)).len(), 2);
    for to in [person_terminal(None), named("terminal")] {
        let id = format!("local-{}", to.person.is_some());
        assert!(matches!(
            ok(&mut engine, 1, send_to(&local, &id, to)),
            Outcome::Sent { .. }
        ));
    }
    assert!(matches!(
        ok(&mut engine, 1, send_to(&local, "remote", person_terminal(Some("desk")))),
        Outcome::TransportQueued { peer, .. } if peer == desk.peer.to_string()
    ));
    assert!(matches!(
        ok(
            &mut engine,
            1,
            send_to(&local, "mine", person_terminal(Some(MACHINE)))
        ),
        Outcome::Sent { .. }
    ));
    assert_eq!(unread(&mut engine, 1, &local), 3);
}

#[test]
fn local_terminal_preference_does_not_hide_another_persons_terminal() {
    let fixture = Fixture::new();
    let mut engine = fixture.open();
    let local = register(&mut engine, 1, "terminal", &fixture.root);
    let other = member(2, "blair", "desk");
    engine
        .sync_remote_members("team", 1, std::slice::from_ref(&other))
        .unwrap();
    engine
        .remote_presence(other.peer, 1, &advertised("terminal", None))
        .unwrap();
    assert!(matches!(
        reply(&mut engine, 1, send_to(&local, "either", named("terminal"))),
        ReplyResult::Error(EngineError::Ambiguous)
    ));
    let theirs = Filter {
        person: Some("blair".into()),
        ..named("terminal")
    };
    assert!(matches!(
        ok(&mut engine, 1, send_to(&local, "theirs", theirs)),
        Outcome::TransportQueued { .. }
    ));
    assert!(matches!(
        ok(
            &mut engine,
            1,
            send_to(&local, "mine", person_terminal(None))
        ),
        Outcome::Sent { .. }
    ));
}

#[test]
fn published_presence_knows_every_name_and_keeps_only_offline_ones() {
    let fixture = Fixture::new();
    let mut engine = fixture.open();
    register(&mut engine, 1, "editor", &fixture.root);
    register(&mut engine, 2, "review", &fixture.root);
    register(&mut engine, 3, "notes", &fixture.root);
    engine.disconnect(ConnectionId(2)).unwrap();
    let presence = engine.local_presence().unwrap();
    let address = |name: &str| format!("{PERSON}/{MACHINE}/{name}");
    let mut known = presence.known.clone();
    known.sort();
    assert_eq!(
        known,
        [address("editor"), address("notes"), address("review")]
    );
    assert_eq!(presence.kept, [address("review")]);
    let mut live: Vec<_> = presence.sessions.iter().map(|s| s.name.as_str()).collect();
    live.sort_unstable();
    assert_eq!(live, ["editor", "notes"]);
    assert!(presence.sessions.iter().all(|s| !s.is_me));
    // With every session offline, each known name is kept and none is live.
    engine.disconnect(ConnectionId(1)).unwrap();
    engine.disconnect(ConnectionId(3)).unwrap();
    let presence = engine.local_presence().unwrap();
    assert!(presence.sessions.is_empty());
    assert_eq!(presence.known.len(), 3);
    assert_eq!(presence.kept.len(), 3);
}

#[test]
fn an_offline_name_stays_reserved_with_its_mail_for_its_own_holder() {
    let fixture = Fixture::new();
    let mut engine = fixture.open();
    let review = register(&mut engine, 1, "review", &fixture.root);
    let writer = register(&mut engine, 2, "writer", &fixture.root);
    engine.disconnect(ConnectionId(1)).unwrap();
    ok(
        &mut engine,
        2,
        send_to(&writer, "while-away", named("review")),
    );
    // Thirty-one days offline, across a restart, the name is still not reusable.
    fixture.advance(31 * 24 * 60 * 60 * 1_000);
    drop(engine);
    let mut engine = fixture.open();
    let retry = Request::Register {
        name: "review".into(),
        repository: fixture.root.clone(),
        share_path: false,
    };
    assert!(matches!(
        reply(&mut engine, 3, retry),
        ReplyResult::Error(EngineError::Unauthorized)
    ));
    let entries = directory(&mut engine, 3, named("review"));
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].online);
    ok(
        &mut engine,
        4,
        Request::Attach {
            auth: review.clone(),
        },
    );
    assert_eq!(unread(&mut engine, 4, &review), 1);
    let mail = peeked(&mut engine, 4, &review);
    assert_eq!(mail[0].message_id, "while-away");
}

#[test]
fn pull_mail_waits_for_the_reader_and_a_failed_handoff_returns_it_unread() {
    let fixture = Fixture::new();
    let mut engine = fixture.open();
    let reader = register(&mut engine, 1, "reader", &fixture.root);
    let writer = register(&mut engine, 2, "writer", &fixture.root);
    let entry = &directory(&mut engine, 2, named("reader"))[0];
    assert!(matches!(entry.capabilities[..], [Capability::Pull]));
    ok(&mut engine, 2, send_to(&writer, "first", named("reader")));
    ok(&mut engine, 2, send_to(&writer, "second", named("reader")));
    fixture.advance(60_000);
    assert_eq!(unread(&mut engine, 1, &reader), 2);
    let offered = peeked(&mut engine, 1, &reader);
    assert_eq!(offered.len(), 2);
    for mail in &offered {
        let attempt_id = mail.attempt_id.unwrap();
        let failed = Request::PushFailed {
            auth: reader.clone(),
            attempt_id,
        };
        assert!(matches!(ok(&mut engine, 1, failed), Outcome::PushFailed));
    }
    assert_eq!(unread(&mut engine, 1, &reader), 2);
    fixture.advance(60_000);
    let again: Vec<_> = peeked(&mut engine, 1, &reader)
        .into_iter()
        .map(|m| m.message_id)
        .collect();
    assert_eq!(again, ["first", "second"]);
}
