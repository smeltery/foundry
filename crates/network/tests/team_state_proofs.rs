#[path = "team_state_support/mod.rs"]
mod support;
use foundry_network::{
    proof::{ProofConfig, ProofVerifier},
    team_state::{Admission, TeamError, TeamState},
};
use std::sync::Arc;
use support::*;
const CLOCK: i64 = 1_000_000;
fn vectors() -> serde_json::Value {
    serde_json::from_str(include_str!("team_state_support/proofs.json")).unwrap()
}
fn token(name: &str) -> String {
    vectors()["tokens"][name].as_str().unwrap().into()
}
fn verifier() -> Arc<ProofVerifier> {
    Arc::new(
        ProofVerifier::from_jwks(
            ProofConfig {
                issuer: "team-issuer".into(),
                audience: "team-audience".into(),
                token_type: "team-proof".into(),
            },
            &serde_json::to_vec(&vectors()["jwks"]).unwrap(),
        )
        .unwrap(),
    )
}
fn setup(proof: &str) -> (Fixture, TeamState) {
    let f = Fixture::new();
    let state = f.state.clone().with_proof_verifier(verifier()).unwrap();
    let state = f
        .admit(&state, &f.owner, &key(2), "alex", f.secret(&state), CLOCK)
        .unwrap();
    let state = state
        .attach_proof(key(2).public(), &token(proof), CLOCK)
        .unwrap();
    (f, state)
}
fn join(
    state: &TeamState,
    actor: &iroh::SecretKey,
    proof: &str,
    owner_only: bool,
) -> Result<TeamState, TeamError> {
    state.admit(
        actor,
        Admission {
            endpoint: key(3).public(),
            person: "ignored",
            machine: "desk",
            secret: &state.document().link.as_ref().unwrap().body.secret,
            proof: Some(proof),
            owner_only,
        },
        CLOCK,
    )
}
#[test]
fn expired_ownership_joins_but_is_not_a_current_badge() {
    let (f, state) = setup("expired_owner");
    assert!(!state.has_current_proof(key(2).public(), CLOCK));
    let joined = join(&state, &f.owner, &token("expired_candidate"), false).unwrap();
    assert!(joined.is_active(key(3).public()));
    assert_eq!(
        joined.document().members[&key(3).public().to_string()]
            .body
            .person,
        "alex"
    );
    assert!(!joined.has_current_proof(key(3).public(), CLOCK));
    assert!(joined.verified_people(CLOCK).is_empty());
    assert!(matches!(
        join(&state, &f.owner, &token("candidate"), true),
        Err(TeamError::OwnerRequired)
    ));
    assert!(join(&state, &key(2), &token("candidate"), true).is_ok());
    let current = state
        .attach_proof(key(2).public(), &token("newer"), CLOCK)
        .unwrap();
    assert_eq!(current.verified_people(CLOCK), ["alex"]);
    assert!(!current.has_current_proof(key(2).public(), 1_500_000));
}
#[test]
fn subject_person_and_active_owner_are_required_and_policy_is_fixed() {
    let (f, state) = setup("owner");
    assert!(matches!(
        join(&state, &f.owner, &token("other_subject"), false),
        Err(TeamError::NameTaken)
    ));
    // An authentic different person may take its own unoccupied name, never alex.
    let other = join(&state, &f.owner, &token("other_person"), false).unwrap();
    assert_eq!(
        other.document().members[&key(3).public().to_string()]
            .body
            .person,
        "other"
    );
    assert!(!other.has_current_proof(key(2).public(), 2_000_000));
    assert!(state.clone().with_proof_verifier(verifier()).is_err());
    let departed = state.departure(&key(2), CLOCK + 1).unwrap();
    assert!(
        departed
            .document()
            .attestations
            .contains_key(&key(2).public().to_string())
    );
    assert!(departed.verified_people(CLOCK).is_empty());
    // Inactive owner no longer occupies the name; admission is the unoccupied branch.
    assert!(join(&departed, &f.owner, &token("other_subject"), true).is_ok());
    assert!(matches!(
        join(&state, &f.owner, &token("owner"), false),
        Err(TeamError::InvalidProof)
    ));
    assert!(matches!(
        join(&state, &f.owner, "broken", false),
        Err(TeamError::InvalidProof)
    ));
}
#[test]
fn replacement_uses_issued_at_then_trimmed_token_and_preserves_rejections() {
    let (_, state) = setup("owner");
    let endpoint = key(2).public();
    let tie = token("tie");
    let base = token("owner");
    let tied = state
        .attach_proof(endpoint, &format!(" \n{tie}\t"), CLOCK)
        .unwrap();
    assert_eq!(
        tied.document().attestations[&endpoint.to_string()],
        base.max(tie)
    );
    let newer = tied.attach_proof(endpoint, &token("newer"), CLOCK).unwrap();
    assert_eq!(
        newer.document().attestations[&endpoint.to_string()],
        token("newer")
    );
    let older = newer
        .attach_proof(endpoint, &token("older"), CLOCK)
        .unwrap();
    assert!(older.document() == newer.document());
    assert!(
        newer
            .attach_proof(endpoint, &token("future"), CLOCK)
            .is_err()
    );
    assert!(newer.attach_proof(endpoint, "invalid", CLOCK).is_err());
    let unknown = key(4).public();
    assert!(
        newer
            .attach_proof(unknown, &token("fourth"), CLOCK)
            .is_err()
    );
}
#[test]
fn proof_first_merge_enables_same_account_and_retains_inactive_attestations() {
    let (f, state) = setup("owner");
    let joined = join(&state, &f.owner, &token("candidate"), false).unwrap();
    let (merged, report) = state
        .merge_from(f.owner.public(), joined.document(), CLOCK)
        .unwrap();
    assert_eq!(report.rejected, 0);
    assert!(merged.is_active(key(3).public()));
    let departed = joined.departure(&key(3), CLOCK + 1).unwrap();
    let (merged, _) = merged
        .merge_from(f.owner.public(), departed.document(), CLOCK + 1)
        .unwrap();
    assert!(!merged.is_active(key(3).public()));
    assert!(
        merged
            .document()
            .attestations
            .contains_key(&key(3).public().to_string())
    );
    let (boot, report) = TeamState::bootstrap_configured(
        &joined.document().id,
        f.owner.public(),
        joined.document(),
        &f.profile,
        CLOCK,
        Some(verifier()),
    )
    .unwrap();
    assert_eq!(report.rejected, 0);
    assert!(boot.document() == joined.document());
    let mut no_member = joined.document().clone();
    no_member.members.remove(&key(3).public().to_string());
    let (boot, _) = TeamState::bootstrap_configured(
        &no_member.id,
        f.owner.public(),
        &no_member,
        &f.profile,
        CLOCK,
        Some(verifier()),
    )
    .unwrap();
    assert!(
        !boot
            .document()
            .attestations
            .contains_key(&key(3).public().to_string())
    );
}
#[test]
fn configured_bad_merge_proofs_do_not_authorize_and_wrong_person_has_no_badge() {
    let (f, state) = setup("owner");
    let joined = join(&state, &f.owner, &token("candidate"), false).unwrap();
    for replacement in [
        "broken".to_owned(),
        token("other_subject"),
        token("other_person"),
    ] {
        let mut incoming = joined.document().clone();
        incoming
            .attestations
            .insert(key(3).public().to_string(), replacement);
        let (merged, report) = state
            .merge_from(f.owner.public(), &incoming, CLOCK)
            .unwrap();
        assert!(!merged.is_active(key(3).public()));
        assert!(report.rejected > 0);
        assert!(state.document().attestations.len() == 1);
    }
    let plain = f
        .admit(
            &f.state,
            &f.owner,
            &key(3),
            "different",
            f.secret(&f.state),
            CLOCK,
        )
        .unwrap()
        .with_proof_verifier(verifier())
        .unwrap();
    let attached = plain
        .attach_proof(key(3).public(), &token("candidate"), CLOCK)
        .unwrap();
    assert!(!attached.has_current_proof(key(3).public(), CLOCK));
    assert!(attached.verified_people(CLOCK).is_empty());
}
#[test]
fn invalid_old_token_cannot_block_valid_replacement_after_clock_change() {
    let (_, state) = setup("owner");
    let endpoint = key(2).public();
    let future = state
        .attach_proof(endpoint, &token("future"), 100_000_000)
        .unwrap();
    let replaced = future
        .attach_proof(endpoint, &token("owner"), CLOCK)
        .unwrap();
    assert_eq!(
        replaced.document().attestations[&endpoint.to_string()],
        token("owner")
    );
    assert!(replaced.has_current_proof(endpoint, CLOCK));
}
#[test]
fn inactive_or_wrong_person_owner_cannot_vouch_for_an_occupied_name() {
    let (f, wrong_person) = setup("owner_other_person");
    assert!(matches!(
        join(&wrong_person, &f.owner, &token("candidate"), false),
        Err(TeamError::NameTaken)
    ));
    let (f, state) = setup("owner");
    let restricted = state
        .issue_invite(&key(2), "sibling", 1, true, CLOCK)
        .unwrap();
    let siblings = f
        .admit(&restricted, &key(2), &key(4), "alex", "sibling", CLOCK)
        .unwrap();
    let departed = siblings.departure(&key(2), CLOCK + 1).unwrap();
    assert!(departed.is_active(key(4).public()));
    assert!(matches!(
        join(&departed, &f.owner, &token("candidate"), false),
        Err(TeamError::NameTaken)
    ));
}
#[test]
fn engine_selected_policy_reverifies_incoming_tokens() {
    let (f, state) = setup("owner");
    let joined = join(&state, &f.owner, &token("candidate"), false).unwrap();
    let other_policy = Arc::new(
        ProofVerifier::from_jwks(
            ProofConfig {
                issuer: "other-issuer".into(),
                audience: "team-audience".into(),
                token_type: "team-proof".into(),
            },
            &serde_json::to_vec(&vectors()["jwks"]).unwrap(),
        )
        .unwrap(),
    );
    let base = f
        .admit(
            &f.state,
            &f.owner,
            &key(2),
            "alex",
            f.secret(&f.state),
            CLOCK,
        )
        .unwrap()
        .with_proof_verifier(other_policy)
        .unwrap();
    let (merged, report) = base
        .merge_from(f.owner.public(), joined.document(), CLOCK)
        .unwrap();
    assert!(report.rejected >= 3);
    assert!(merged.document().attestations.is_empty());
    assert!(!merged.is_active(key(3).public()));
    assert!(matches!(
        join(&base, &f.owner, &token("candidate"), false),
        Err(TeamError::InvalidProof)
    ));
}
