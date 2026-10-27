#[path = "team_state_support/mod.rs"]
mod support;
use foundry_network::{
    signing::SignedRecord,
    team::InviteRecord,
    team_state::{Admission, Formation, TeamError, TeamState},
};
use support::*;

#[test]
fn formation_signs_complete_state_and_bootstrap_preserves_supported_fields() {
    let fixture = Fixture::new();
    let doc = fixture.state.document();
    assert_eq!(doc.id, "01".repeat(16));
    assert_eq!(doc.link.as_ref().unwrap().body.secret, "02".repeat(16));
    assert!(fixture.state.is_active(fixture.owner.public()));
    for member in doc.members.values() {
        member.verify(&doc.id, &fixture.profile).unwrap();
    }
    for member in doc.admissions.values() {
        member.verify(&doc.id, &fixture.profile).unwrap();
    }
    doc.meta.verify(&doc.id, &fixture.profile).unwrap();
    doc.link
        .as_ref()
        .unwrap()
        .verify(&doc.id, &fixture.profile)
        .unwrap();
    let invitation = &doc.invites[&hash(fixture.secret(&fixture.state))];
    assert!(invitation.body.reusable);
    assert_eq!(invitation.body.expires, i64::MAX / 4);
    assert!(invitation.body.person.is_none() && invitation.body.used_by.is_none());
    invitation.verify(&doc.id, &fixture.profile).unwrap();
    assert!(fixture.boot(doc).document() == doc);
}
#[test]
fn invitation_lifetime_clamps_and_pruning_keep_exact_boundary() {
    let fixture = Fixture::new();
    let short = fixture
        .state
        .issue_invite(&fixture.owner, "short", 0, true, NOW)
        .unwrap();
    assert_eq!(
        short.document().invites[&hash("short")].body.expires,
        NOW + 3_600_000
    );
    assert_eq!(
        short.document().invites[&hash("short")]
            .body
            .person
            .as_deref(),
        Some("alice")
    );
    let long = short
        .issue_invite(&fixture.owner, "long", u64::MAX, false, NOW)
        .unwrap();
    assert_eq!(
        long.document().invites[&hash("long")].body.expires,
        NOW + 720 * 3_600_000
    );
    let mut doc = long.document().clone();
    for (secret, expires) in [
        ("old", NOW - 7 * 86_400_000),
        ("keep", NOW - 7 * 86_400_000 + 1),
    ] {
        let record = SignedRecord::sign(
            InviteRecord {
                hash: hash(secret),
                expires,
                person: None,
                used_by: None,
                ts: 1,
                reusable: false,
            },
            &fixture.owner,
            &doc.id,
            &fixture.profile,
        )
        .unwrap();
        doc.invites.insert(hash(secret), record);
    }
    let pruned = fixture
        .boot(&doc)
        .issue_invite(&fixture.owner, "new", 24, false, NOW)
        .unwrap();
    assert!(!pruned.document().invites.contains_key(&hash("old")));
    assert!(pruned.document().invites.contains_key(&hash("keep")));
    assert!(matches!(
        fixture
            .state
            .issue_invite(&fixture.owner, "x", 24, false, i64::MAX),
        Err(TeamError::ClockOverflow)
    ));
}
#[test]
fn single_use_retry_order_and_inactive_replay_are_preserved() {
    let fixture = Fixture::new();
    let bob = key(2);
    let other = key(3);
    let issued = fixture
        .state
        .issue_invite(&fixture.owner, "once", 1, false, NOW)
        .unwrap();
    let joined = fixture
        .admit(&issued, &fixture.owner, &bob, "bob", "once", NOW)
        .unwrap();
    let consumed = &joined.document().invites[&hash("once")];
    assert_eq!(consumed.body.used_by, Some(bob.public()));
    consumed
        .verify(&joined.document().id, &fixture.profile)
        .unwrap();
    assert!(matches!(
        fixture.admit(&joined, &fixture.owner, &other, "other", "once", NOW),
        Err(TeamError::InviteUsed)
    ));
    let retry = fixture
        .admit(
            &joined,
            &fixture.owner,
            &bob,
            "renamed",
            "once",
            NOW + 4_000_000,
        )
        .unwrap();
    assert_eq!(
        retry.document().members[&bob.public().to_string()]
            .body
            .person,
        "renamed"
    );
    assert!(retry.document().invites[&hash("once")] == *consumed);
    let removed = joined
        .remove(
            &fixture.owner,
            &[bob.public()],
            NOW + 1,
            &mut FixedEntropy(4),
        )
        .unwrap();
    assert!(matches!(
        fixture.admit(&removed, &fixture.owner, &bob, "bob", "once", NOW + 2),
        Err(TeamError::InviteUsed)
    ));
    assert!(
        issued.document().invites[&hash("once")]
            .body
            .used_by
            .is_none()
    );
}
#[test]
fn unrestricted_and_own_machine_name_rules_remain_distinct() {
    let fixture = Fixture::new();
    let bob = key(2);
    let sibling = key(3);
    let joined = fixture
        .admit(
            &fixture.state,
            &fixture.owner,
            &bob,
            "bob",
            fixture.secret(&fixture.state),
            NOW,
        )
        .unwrap();
    let restricted = joined.issue_invite(&bob, "own", 24, true, NOW).unwrap();
    assert!(matches!(
        fixture.admit(&restricted, &fixture.owner, &sibling, "bob", "own", NOW),
        Err(TeamError::OwnerRequired)
    ));
    assert!(matches!(
        fixture.admit(&restricted, &bob, &sibling, "other", "own", NOW),
        Err(TeamError::WrongPerson)
    ));
    let admitted = fixture
        .admit(&restricted, &bob, &sibling, "bob", "own", NOW)
        .unwrap();
    assert!(admitted.is_active(sibling.public()));
    assert!(matches!(
        fixture.admit(
            &joined,
            &fixture.owner,
            &sibling,
            "bob",
            fixture.secret(&joined),
            NOW
        ),
        Err(TeamError::NameTaken)
    ));
    // owner_only is a proof fallback control, not an unconditional restriction.
    let accepted = joined
        .admit(
            &fixture.owner,
            Admission {
                endpoint: sibling.public(),
                person: "other",
                machine: "desk",
                secret: fixture.secret(&joined),
                proof: None,
                owner_only: true,
            },
            NOW,
        )
        .unwrap();
    assert!(accepted.is_active(sibling.public()));
}
#[test]
fn reusable_link_rotation_expires_old_link_and_preserves_new_invite() {
    let fixture = Fixture::new();
    let first = key(2);
    let second = key(3);
    let original = fixture.secret(&fixture.state).to_string();
    let once = fixture
        .admit(
            &fixture.state,
            &fixture.owner,
            &first,
            "bob",
            &original,
            NOW,
        )
        .unwrap();
    let twice = fixture
        .admit(&once, &fixture.owner, &second, "carol", &original, NOW)
        .unwrap();
    assert!(
        twice.document().invites[&hash(&original)]
            .body
            .used_by
            .is_none()
    );
    let rotated = twice
        .rotate_link(&fixture.owner, NOW, &mut FixedEntropy(8))
        .unwrap();
    assert_eq!(
        rotated.document().invites[&hash(&original)].body.expires,
        NOW
    );
    assert!(
        rotated.document().link.as_ref().unwrap().body.ts
            > twice.document().link.as_ref().unwrap().body.ts
    );
    assert!(matches!(
        fixture.admit(&rotated, &fixture.owner, &key(4), "dave", &original, NOW),
        Err(TeamError::LinkReplaced)
    ));
    assert!(
        fixture
            .admit(
                &rotated,
                &fixture.owner,
                &key(4),
                "dave",
                fixture.secret(&rotated),
                NOW
            )
            .is_ok()
    );
}
#[test]
fn invitation_expiry_and_inactive_signer_precede_retries() {
    let fixture = Fixture::new();
    let bob = key(2);
    let carol = key(3);
    let once = fixture
        .state
        .issue_invite(&fixture.owner, "expired", 1, false, NOW)
        .unwrap();
    assert!(matches!(
        fixture.admit(
            &once,
            &fixture.owner,
            &bob,
            "bob",
            "expired",
            NOW + 3_600_000
        ),
        Err(TeamError::InviteExpired)
    ));
    let joined = fixture
        .admit(
            &fixture.state,
            &fixture.owner,
            &bob,
            "bob",
            fixture.secret(&fixture.state),
            NOW,
        )
        .unwrap();
    let issued = joined.issue_invite(&bob, "signed", 24, false, NOW).unwrap();
    let admitted = fixture
        .admit(&issued, &bob, &carol, "carol", "signed", NOW)
        .unwrap();
    let inactive = admitted.departure(&bob, NOW + 1).unwrap();
    assert!(matches!(
        fixture.admit(
            &inactive,
            &fixture.owner,
            &carol,
            "carol",
            "signed",
            NOW + 2
        ),
        Err(TeamError::InactiveInviter)
    ));
}
#[test]
fn entropy_failure_and_bad_actor_leave_original_unchanged() {
    let fixture = Fixture::new();
    let before = fixture.state.document().clone();
    assert!(matches!(
        fixture
            .state
            .rotate_link(&fixture.owner, NOW, &mut FailedEntropy),
        Err(TeamError::Entropy)
    ));
    assert!(matches!(
        fixture.state.issue_invite(&key(2), "x", 1, false, NOW),
        Err(TeamError::Unauthorized)
    ));
    assert!(matches!(
        fixture.state.departure(&key(2), NOW),
        Err(TeamError::Unauthorized)
    ));
    assert!(fixture.state.document() == &before);
    assert!(matches!(
        TeamState::found(
            Formation {
                name: "x",
                person: "alice",
                machine: "desk"
            },
            &fixture.owner,
            &fixture.profile,
            NOW,
            &mut FailedEntropy
        ),
        Err(TeamError::Entropy)
    ));
}
