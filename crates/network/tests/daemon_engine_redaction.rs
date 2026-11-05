//! Invitation text placed in session status or doing never leaves as a ticket.
#![cfg(unix)]
#[allow(dead_code)]
#[path = "daemon_engine_support/mod.rs"]
mod support;
use foundry_network::{
    WireProfile,
    daemon_engine::*,
    ticket::{Ticket, TicketCodec},
};
use support::*;

fn profile() -> WireProfile {
    WireProfile {
        team_alpn: b"redaction/team".to_vec(),
        join_alpn: b"redaction/join".to_vec(),
        protocol: 1,
        signing_prefix: "redaction/sign/".into(),
        invitation_endpoint_context: "redaction endpoint".into(),
        invitation_secret_context: "redaction secret".into(),
        ticket_prefixes: vec!["rdx-".into(), "oldrdx-".into()],
    }
}
/// A real encoded invitation, so redaction is checked against actual output.
fn invitation() -> String {
    let profile = profile();
    let ticket = Ticket {
        team: "ab".repeat(16),
        secret: "cd".repeat(16),
        name: "Redaction fixture".into(),
        relay: None,
        hint_relay: None,
        hint_ips: vec!["127.0.0.1:4100".parse().unwrap()],
        peers: vec![peer(9)],
    };
    TicketCodec::new(&profile, None)
        .unwrap()
        .encode(&ticket)
        .unwrap()
}
fn secret_fragment(text: &str) -> &str {
    // Everything after the prefix carries team and secret bytes.
    &text[text.find('-').unwrap() + 1..]
}
fn set_status(engine: &mut Engine, connection: u64, auth: &Auth, status: &str, doing: &str) {
    let request = Request::SetStatus {
        auth: auth.clone(),
        status: status.into(),
        doing: doing.into(),
    };
    assert!(matches!(
        ok(engine, connection, request),
        Outcome::StatusUpdated
    ));
}

#[test]
fn status_and_doing_are_redacted_in_directory_presence_and_storage() {
    let fixture = Fixture::new();
    let link = invitation();
    let old_prefix = format!("old{link}");
    let doing = format!("join us: {link}  then ping [{old_prefix}](https://example.test)");
    let status = format!("sharing {link}");
    let mut engine = fixture.open();
    engine.redact_tickets(profile()).unwrap();
    let auth = register(&mut engine, 1, "terminal", &fixture.root);
    set_status(&mut engine, 1, &auth, &status, &doing);
    let expected_doing = "join us: (team link)  then ping (team link)";
    let check = |engine: &mut Engine| {
        let entry = &directory(engine, 1, Filter::default())[0];
        assert_eq!(entry.doing, expected_doing);
        assert_eq!(entry.status, "sharing (team link)");
        let presence = engine.local_presence().unwrap();
        let shared = &presence.sessions[0];
        assert_eq!(shared.doing.as_deref(), Some(expected_doing));
        assert_eq!(shared.status, "sharing (team link)");
        let serialized = serde_json::to_string(&presence).unwrap();
        assert!(!serialized.contains(secret_fragment(&link)));
    };
    check(&mut engine);
    // Storage holds the redacted text: a reopened engine without redaction
    // configured still cannot reproduce the invitation.
    drop(engine);
    let mut engine = fixture.open();
    ok(&mut engine, 1, Request::Attach { auth: auth.clone() });
    check(&mut engine);
    for file in std::fs::read_dir(&fixture.root).unwrap() {
        let bytes = std::fs::read(file.unwrap().path()).unwrap_or_default();
        let fragment = secret_fragment(&link).as_bytes();
        assert!(!bytes.windows(fragment.len()).any(|w| w == fragment));
    }
}

#[test]
fn advertised_and_previously_stored_text_is_redacted_on_the_way_out() {
    let fixture = Fixture::new();
    let link = invitation();
    let mut engine = fixture.open();
    let auth = register(&mut engine, 1, "terminal", &fixture.root);
    // Written before redaction was configured, as an older daemon would have.
    set_status(&mut engine, 1, &auth, "ready", &link);
    engine.redact_tickets(profile()).unwrap();
    let desk = member(1, "blair", "desk");
    engine
        .sync_remote_members("team", 1, std::slice::from_ref(&desk))
        .unwrap();
    let remote_doing = format!("try {link}");
    engine
        .remote_presence(desk.peer, 1, &advertised("review", Some(&remote_doing)))
        .unwrap();
    let entries = directory(&mut engine, 1, Filter::default());
    let doing: Vec<_> = entries.iter().map(|e| e.doing.as_str()).collect();
    assert_eq!(doing.len(), 2);
    assert!(doing.contains(&"(team link)"));
    assert!(doing.contains(&"try (team link)"));
    let presence = engine.local_presence().unwrap();
    assert_eq!(presence.sessions[0].doing.as_deref(), Some("(team link)"));
    // Plain text, and a prefix without a secret-bearing payload, pass unchanged.
    set_status(
        &mut engine,
        1,
        &auth,
        "rdx-",
        "reviewing the rdx-short branch",
    );
    let entry = &directory(&mut engine, 1, named_terminal())[0];
    assert_eq!(entry.status, "rdx-");
    assert_eq!(entry.doing, "reviewing the rdx-short branch");
}

#[test]
fn an_invalid_redaction_profile_is_refused() {
    let fixture = Fixture::new();
    let mut engine = fixture.open();
    let mut overlapping = profile();
    overlapping.ticket_prefixes = vec!["rdx-".into(), "rdx-old-".into()];
    assert_eq!(
        engine.redact_tickets(overlapping).unwrap_err(),
        EngineError::InvalidInput
    );
}

fn named_terminal() -> Filter {
    Filter {
        name: Some("terminal".into()),
        ..Filter::default()
    }
}
