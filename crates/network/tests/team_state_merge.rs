#[path = "team_state_support/mod.rs"]
mod support;
use foundry_network::{
    signing::SignedRecord,
    team::LinkRecord,
    team_state::{Admission, TeamError, TeamState},
};
use support::*;

#[test]
fn removal_rotates_and_filters_all_secret_state_before_disconnect() {
    let fixture = Fixture::new();
    let bob = key(2);
    let carol = key(3);
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
    let joined = fixture
        .admit(
            &joined,
            &fixture.owner,
            &carol,
            "carol",
            fixture.secret(&joined),
            NOW,
        )
        .unwrap();
    let old = fixture.secret(&joined).to_string();
    let before = joined.document().clone();
    assert!(matches!(
        joined.remove(&fixture.owner, &[bob.public()], NOW, &mut FailedEntropy),
        Err(TeamError::Entropy)
    ));
    assert!(joined.document() == &before);
    let removed = joined
        .remove(
            &fixture.owner,
            &[bob.public(), carol.public()],
            NOW + 1,
            &mut FixedEntropy(9),
        )
        .unwrap();
    assert!(!removed.is_active(bob.public()) && !removed.is_active(carol.public()));
    assert_ne!(fixture.secret(&removed), old);
    let filtered = removed.document_for(bob.public()).unwrap();
    assert!(filtered.link.is_none());
    assert!(filtered.invites.is_empty());
    assert_eq!(filtered.members.len(), 3);
    assert_eq!(filtered.admissions.len(), 3);
    assert!(removed.document_for(key(8).public()).is_none());
    assert!(removed.document_for(fixture.owner.public()).unwrap() == *removed.document());
    assert!(matches!(
        joined.remove(
            &fixture.owner,
            &[fixture.owner.public()],
            NOW,
            &mut FixedEntropy(8)
        ),
        Err(TeamError::Invalid)
    ));
    assert!(matches!(
        joined.remove(&fixture.owner, &[], NOW, &mut FixedEntropy(8)),
        Err(TeamError::Invalid)
    ));
}
#[test]
fn departure_keeps_link_for_active_peers_but_never_shows_it_to_departed_peer() {
    let fixture = Fixture::new();
    let bob = key(2);
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
    let departed = joined.departure(&fixture.owner, NOW + 1).unwrap();
    assert!(!departed.is_active(fixture.owner.public()));
    assert!(departed.is_active(bob.public()));
    assert!(departed.document().link == joined.document().link);
    assert!(departed.document().invites == joined.document().invites);
    assert!(
        departed
            .document_for(fixture.owner.public())
            .unwrap()
            .link
            .is_none()
    );
    assert!(departed.document_for(bob.public()).unwrap().link.is_some());
    assert!(matches!(
        departed.rotate_link(&fixture.owner, NOW + 2, &mut FixedEntropy(10)),
        Err(TeamError::Unauthorized)
    ));
}
#[test]
fn proof_bearing_input_is_explicitly_unsupported_and_never_cleared_or_applied() {
    let fixture = Fixture::new();
    let before = fixture.state.document().clone();
    let mut supplied = before.clone();
    supplied.attestations.insert(
        fixture.owner.public().to_string(),
        "fixture.unverified.proof".into(),
    );
    assert!(matches!(
        TeamState::bootstrap(
            &supplied.id,
            supplied.founder,
            &supplied,
            &fixture.profile,
            NOW
        ),
        Err(TeamError::ProofVerificationUnavailable)
    ));
    assert!(matches!(
        fixture
            .state
            .merge_from(fixture.owner.public(), &supplied, NOW),
        Err(TeamError::ProofVerificationUnavailable)
    ));
    assert!(matches!(
        fixture.state.admit(
            &fixture.owner,
            Admission {
                endpoint: key(2).public(),
                person: "bob",
                machine: "desk",
                secret: fixture.secret(&fixture.state),
                proof: Some("fixture.proof"),
                owner_only: true
            },
            NOW
        ),
        Err(TeamError::ProofVerificationUnavailable)
    ));
    assert_eq!(supplied.attestations.len(), 1);
    assert!(fixture.state.document() == &before);
}
#[test]
fn historically_admitted_inactive_signer_can_sign_updates_but_cannot_connect() {
    let fixture = Fixture::new();
    let bob = key(2);
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
    let removed = joined
        .remove(
            &fixture.owner,
            &[bob.public()],
            NOW + 1,
            &mut FixedEntropy(8),
        )
        .unwrap();
    let mut remote = removed.document().clone();
    remote.link = Some(
        SignedRecord::sign(
            LinkRecord {
                secret: "fixture-historical-link".into(),
                ts: NOW + 5,
            },
            &bob,
            &remote.id,
            &fixture.profile,
        )
        .unwrap(),
    );
    assert!(matches!(
        removed.merge_from(bob.public(), &remote, NOW + 5),
        Err(TeamError::Unauthorized)
    ));
    let (merged, report) = removed
        .merge_from(fixture.owner.public(), &remote, NOW + 5)
        .unwrap();
    assert!(report.changed);
    assert!(merged.document().link == remote.link);
    assert!(!merged.is_active(bob.public()));
}
#[test]
fn forged_future_wrong_map_and_wrong_team_updates_are_rejected() {
    let fixture = Fixture::new();
    let before = fixture.state.document().clone();
    let rotated = fixture
        .state
        .rotate_link(&fixture.owner, NOW + 1, &mut FixedEntropy(8))
        .unwrap();
    let mut forged = rotated.document().clone();
    forged.link.as_mut().unwrap().body.secret.push('x');
    let (merged, report) = fixture
        .state
        .merge_from(fixture.owner.public(), &forged, NOW)
        .unwrap();
    assert_eq!(report.rejected, 1);
    assert!(merged.document().link == before.link);
    let mut future = before.clone();
    future.link = Some(
        SignedRecord::sign(
            LinkRecord {
                secret: "fixture-future".into(),
                ts: NOW + 86_400_001,
            },
            &fixture.owner,
            &future.id,
            &fixture.profile,
        )
        .unwrap(),
    );
    let (merged, report) = fixture
        .state
        .merge_from(fixture.owner.public(), &future, NOW)
        .unwrap();
    assert_eq!(report.rejected, 1);
    assert!(merged.document() == &before);
    let mut wrong = before.clone();
    let invitation = wrong.invites.values().next().unwrap().clone();
    wrong.invites.insert("wrong-map".into(), invitation);
    assert_eq!(
        fixture
            .state
            .merge_from(fixture.owner.public(), &wrong, NOW)
            .unwrap()
            .1
            .rejected,
        1
    );
    wrong.id = "foreign".into();
    assert!(matches!(
        fixture
            .state
            .merge_from(fixture.owner.public(), &wrong, NOW),
        Err(TeamError::DifferentTeam)
    ));
}
#[test]
fn independent_peers_consuming_one_invite_are_not_global_exactly_once() {
    let fixture = Fixture::new();
    let base = fixture
        .state
        .issue_invite(&fixture.owner, "once", 24, false, NOW)
        .unwrap();
    let bob = key(2);
    let carol = key(3);
    let left = fixture
        .admit(&base, &fixture.owner, &bob, "bob", "once", NOW + 1)
        .unwrap();
    let right = fixture
        .admit(&base, &fixture.owner, &carol, "carol", "once", NOW + 2)
        .unwrap();
    let (merged, _) = left
        .merge_from(fixture.owner.public(), right.document(), NOW + 2)
        .unwrap();
    assert!(merged.is_active(bob.public()) && merged.is_active(carol.public()));
    assert_eq!(
        merged.document().invites[&hash("once")].body.used_by,
        Some(carol.public())
    );
}
#[test]
fn stale_secret_state_cannot_undo_rotation_and_merge_is_idempotent() {
    let fixture = Fixture::new();
    let rotated = fixture
        .state
        .rotate_link(&fixture.owner, NOW + 1, &mut FixedEntropy(8))
        .unwrap();
    let (replayed, report) = rotated
        .merge_from(fixture.owner.public(), fixture.state.document(), NOW + 1)
        .unwrap();
    assert!(!report.changed);
    assert!(replayed.document() == rotated.document());
    let (again, report) = replayed
        .merge_from(fixture.owner.public(), replayed.document(), NOW + 1)
        .unwrap();
    assert!(!report.changed);
    assert!(again.document() == replayed.document());
}

#[test]
fn departure_preserves_existing_same_person_siblings_without_rebootstrap() {
    let fixture = Fixture::new();
    let bob = key(2);
    let first = key(3);
    let second = key(4);
    let mut state = fixture
        .admit(
            &fixture.state,
            &fixture.owner,
            &bob,
            "bob",
            fixture.secret(&fixture.state),
            NOW,
        )
        .unwrap();
    for (sibling, secret) in [(&first, "first"), (&second, "second")] {
        state = state.issue_invite(&bob, secret, 24, true, NOW).unwrap();
        state = fixture
            .admit(&state, &bob, sibling, "bob", secret, NOW)
            .unwrap();
    }
    let departed = state.departure(&bob, NOW + 1).unwrap();
    assert!(!departed.is_active(bob.public()));
    assert!(departed.is_active(first.public()) && departed.is_active(second.public()));
    assert_eq!(departed.document().members.len(), 4);
}
