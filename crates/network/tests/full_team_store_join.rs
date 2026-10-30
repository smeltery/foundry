#![cfg(unix)]
#[path = "full_team_store_support/mod.rs"]
mod support;
use foundry_network::{
    full_team_store::{FullTeamStore, JoinTrust, StoreError},
    team::TeamDocument,
    team_state::Admission,
};
use support::{Fixture, NOW, formation};

fn admitted(issuer: &Fixture, joiner: &Fixture) -> TeamDocument {
    admitted_with_secret(issuer, joiner, "join-secret")
}

fn admitted_with_secret(issuer: &Fixture, joiner: &Fixture, secret: &str) -> TeamDocument {
    let mut store = issuer.create();
    let empty = store.load(NOW).unwrap();
    let pending = store.prepare_formation(&empty, formation(), NOW).unwrap();
    let loaded = store.commit_formation(pending).unwrap();
    let pending = store
        .prepare_invite(&loaded, secret, 24, false, NOW)
        .unwrap();
    store.commit_transition(pending, NOW).unwrap();
    let base = store.load(NOW).unwrap();
    let pending = store
        .prepare_admission(
            &base,
            Admission {
                endpoint: joiner.identity.endpoint_id(),
                person: "bob",
                machine: "laptop",
                secret,
                proof: None,
                owner_only: false,
            },
            NOW,
        )
        .unwrap();
    store.commit_transition(pending, NOW).unwrap();
    store.load(NOW).unwrap().document().unwrap().clone()
}

#[test]
fn short_code_introduces_team_through_derived_endpoint_and_consumed_invitation() {
    use foundry_network::{
        code_invite::{CodePolicy, Codebook},
        full_team_store::CodeJoinTrust,
    };
    let (issuer, joiner) = (Fixture::new(), Fixture::new());
    let bytes = b"amber\ncedar\n";
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .try_into()
        .unwrap();
    let policy = CodePolicy::new(
        Codebook::from_bytes(bytes, digest).unwrap(),
        &issuer.base.profile,
    )
    .unwrap();
    let code = policy.normalize("amber-amber-amber-amber").unwrap();
    let derived = policy.derive(&code).unwrap();
    let document = admitted_with_secret(&issuer, &joiner, derived.bearer_secret());
    assert!(
        !document
            .members
            .contains_key(&derived.endpoint_key().public().to_string())
    );
    assert!(
        policy
            .authenticate_endpoint(&code, issuer.identity.endpoint_id())
            .is_err()
    );
    let anchor = policy
        .authenticate_endpoint(&code, derived.endpoint_key().public())
        .unwrap();
    let mut store = new_store(&joiner, &issuer);
    let empty = store.load(NOW).unwrap();
    let wrong = policy.normalize("cedar-cedar-cedar-cedar").unwrap();
    let other = policy.derive(&wrong).unwrap();
    let wrong_anchor = policy
        .authenticate_endpoint(&wrong, other.endpoint_key().public())
        .unwrap();
    assert!(
        store
            .prepare_code_join(
                &empty,
                &document,
                CodeJoinTrust {
                    anchor: &wrong_anchor,
                    team: None,
                    founder: None
                },
                NOW
            )
            .is_err()
    );
    let pending = store
        .prepare_code_join(
            &empty,
            &document,
            CodeJoinTrust {
                anchor: &anchor,
                team: None,
                founder: None,
            },
            NOW,
        )
        .unwrap();
    let loaded = store.commit_join(pending).unwrap();
    assert!(loaded.document() == Some(&document));
}

fn new_store(joiner: &Fixture, issuer: &Fixture) -> FullTeamStore {
    // The caller selected the invitation's protocol/trust configuration; it is
    // not taken from unverified reply fields.
    FullTeamStore::create_new(
        &joiner.base.root,
        joiner.identity.clone(),
        issuer.policy(),
        support::WAIT,
    )
    .unwrap()
}

#[test]
fn joined_snapshot_is_lossless_and_durable_with_optional_founder_pin() {
    for pin in [false, true] {
        let (issuer, joiner) = (Fixture::new(), Fixture::new());
        let document = admitted(&issuer, &joiner);
        let mut store = new_store(&joiner, &issuer);
        let base = store.load(NOW).unwrap();
        let trust = JoinTrust {
            team: &document.id,
            founder: pin.then_some(document.founder),
            inviter: issuer.identity.endpoint_id(),
        };
        let pending = store.prepare_join(&base, &document, trust, NOW).unwrap();
        assert!(store.load(NOW).unwrap().document().is_none());
        let joined = store.commit_join(pending).unwrap();
        assert_eq!(joined.revision(), 1);
        assert!(joined.document() == Some(&document));
        drop(store);
        let mut reopened = FullTeamStore::open_existing(
            &joiner.base.root,
            joiner.identity.clone(),
            issuer.policy(),
            support::WAIT,
        )
        .unwrap();
        assert!(reopened.load(NOW).unwrap().document() == Some(&document));
    }
}

#[test]
fn wrong_team_root_inviter_and_missing_or_corrupt_admission_cannot_join() {
    let (issuer, joiner) = (Fixture::new(), Fixture::new());
    let document = admitted(&issuer, &joiner);
    let mut store = new_store(&joiner, &issuer);
    let base = store.load(NOW).unwrap();
    let outsider = iroh::SecretKey::generate().public();
    for trust in [
        JoinTrust {
            team: "different",
            founder: None,
            inviter: issuer.identity.endpoint_id(),
        },
        JoinTrust {
            team: &document.id,
            founder: Some(outsider),
            inviter: issuer.identity.endpoint_id(),
        },
        JoinTrust {
            team: &document.id,
            founder: None,
            inviter: outsider,
        },
        JoinTrust {
            team: &document.id,
            founder: None,
            inviter: joiner.identity.endpoint_id(),
        },
    ] {
        assert!(store.prepare_join(&base, &document, trust, NOW).is_err());
    }
    let local = joiner.identity.endpoint_id().to_string();
    let mut missing = document.clone();
    missing.members.remove(&local);
    let mut corrupt = document.clone();
    corrupt.members.get_mut(&local).unwrap().sig.push('x');
    for incoming in [missing, corrupt] {
        let trust = JoinTrust {
            team: &document.id,
            founder: None,
            inviter: issuer.identity.endpoint_id(),
        };
        assert!(store.prepare_join(&base, &incoming, trust, NOW).is_err());
    }
    assert_eq!(store.load(NOW).unwrap().revision(), 0);
    assert!(store.load(NOW).unwrap().document().is_none());
}

#[test]
fn stale_join_does_not_replace_concurrent_formation() {
    let (issuer, joiner) = (Fixture::new(), Fixture::new());
    let document = admitted(&issuer, &joiner);
    let mut store = new_store(&joiner, &issuer);
    let base = store.load(NOW).unwrap();
    let pending = store
        .prepare_join(
            &base,
            &document,
            JoinTrust {
                team: &document.id,
                founder: None,
                inviter: issuer.identity.endpoint_id(),
            },
            NOW,
        )
        .unwrap();
    let formation = store.prepare_formation(&base, formation(), NOW).unwrap();
    let formed = store.commit_formation(formation).unwrap();
    let expected = formed.document().unwrap().id.clone();
    assert!(matches!(
        store.commit_join(pending),
        Err(StoreError::Conflict)
    ));
    assert_eq!(store.load(NOW).unwrap().document().unwrap().id, expected);
}
