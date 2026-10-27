use super::tests::{Fixture, add, seal};
use super::*;
use iroh::SecretKey;
const NOW: i64 = 1_000_000;
fn migrate(f: &Fixture) -> Result<FullTeamStore> {
    FullTeamStore::migrate_v1(
        &f.root,
        f.identity.clone(),
        f.policy(),
        Duration::from_secs(1),
        NOW,
    )
}
#[test]
fn explicit_migration_preserves_empty_and_historical_documents_exactly() {
    for history in 0..3 {
        let f = Fixture::new();
        let mut store = f.store();
        if history > 0 {
            let bob = SecretKey::from_bytes(&[2; 32]);
            let first = SecretKey::from_bytes(&[3; 32]);
            let second = SecretKey::from_bytes(&[4; 32]);
            let state = f.formation();
            let mut state = add(
                &state,
                f.identity.secret_key(),
                &bob,
                "bob",
                &state.document().link.as_ref().unwrap().body.secret,
            );
            if history == 1 {
                for (key, secret) in [(&first, "one"), (&second, "two")] {
                    state = state.issue_invite(&bob, secret, 1, true, NOW).unwrap();
                    state = add(&state, &bob, key, "bob", secret);
                }
                state = state.departure(&bob, NOW + 1).unwrap();
            } else {
                state = add(
                    &state,
                    f.identity.secret_key(),
                    &first,
                    "carol",
                    &state.document().link.as_ref().unwrap().body.secret,
                );
                state = state
                    .issue_invite(f.identity.secret_key(), "retry", 1, false, NOW)
                    .unwrap();
                state = add(&state, f.identity.secret_key(), &second, "other", "retry");
                state = add(&state, f.identity.secret_key(), &second, "carol", "retry");
            }
            seal(&store, state.document().clone());
        }
        let expected = store.load(NOW).unwrap();
        let revision = expected.revision();
        let incarnation = expected.checkpoint.incarnation;
        let parent = expected.checkpoint.parent;
        legacy::v1_fixture(&store);
        drop(store);
        assert!(matches!(
            FullTeamStore::open_existing(
                &f.root,
                f.identity.clone(),
                f.policy(),
                Duration::from_secs(1)
            ),
            Err(StoreError::Schema)
        ));
        let mut migrated = migrate(&f).unwrap();
        let loaded = migrated.load(NOW).unwrap();
        assert_eq!(loaded.revision(), revision);
        assert_eq!(loaded.checkpoint.incarnation, incarnation);
        assert_eq!(loaded.checkpoint.parent, parent);
        assert!(loaded.document() == expected.document());
        assert!(matches!(migrate(&f), Err(StoreError::Schema)));
        if history > 0 {
            let pending = migrated.prepare_rotation(&loaded, NOW + 2).unwrap();
            match migrated.commit_transition(pending, NOW + 2).unwrap() {
                CommitOutcome::Active(next) => {
                    assert!(next.document().unwrap().members == loaded.document().unwrap().members);
                    drop(migrated);
                    let mut reopened = FullTeamStore::open_existing(
                        &f.root,
                        f.identity.clone(),
                        f.policy(),
                        Duration::from_secs(1),
                    )
                    .unwrap();
                    assert!(reopened.load(NOW + 2).unwrap().document() == next.document());
                }
                _ => panic!("expected active"),
            }
        }
    }
}
#[test]
fn migration_rolls_back_on_injected_write_failure_and_reports_sync_uncertainty() {
    let f = Fixture::new();
    let store = f.store();
    legacy::v1_fixture(&store);
    drop(store);
    legacy::FAIL_AFTER_WRITE.with(|flag| flag.set(true));
    assert!(matches!(migrate(&f), Err(StoreError::Storage)));
    let connection = Connection::open(f.root.join(FULL_TEAM_DATABASE)).unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
    drop(connection);
    legacy::FAIL_SYNC.with(|flag| flag.set(true));
    assert!(matches!(migrate(&f), Err(StoreError::Indeterminate)));
    let mut reopened = FullTeamStore::open_existing(
        &f.root,
        f.identity.clone(),
        f.policy(),
        Duration::from_secs(1),
    )
    .unwrap();
    assert!(reopened.load(NOW).unwrap().document().is_none());
}
#[test]
fn migration_rejects_bad_seals_and_policy_without_rewriting_v1() {
    for kind in 0..2 {
        let f = Fixture::new();
        let store = f.store();
        legacy::v1_fixture(&store);
        if kind == 0 {
            store
                .connection
                .execute("UPDATE checkpoint SET signature=zeroblob(64)", [])
                .unwrap();
        }
        drop(store);
        let result = if kind == 0 {
            migrate(&f)
        } else {
            let mut profile = f.profile.clone();
            profile.protocol += 1;
            FullTeamStore::migrate_v1(
                &f.root,
                f.identity.clone(),
                StorePolicy::new(profile, None).unwrap(),
                Duration::from_secs(1),
                NOW,
            )
        };
        assert!(result.is_err());
        let connection = Connection::open(f.root.join(FULL_TEAM_DATABASE)).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }
}
#[test]
fn migration_changes_base_digest_and_rejects_pre_migration_candidate() {
    let f = Fixture::new();
    let mut original = f.store();
    let empty = original.load(NOW).unwrap();
    let mut pending = original
        .prepare_formation(
            &empty,
            Formation {
                name: "group",
                person: "alice",
                machine: "desk",
            },
            NOW,
        )
        .unwrap();
    pending.base = legacy::v1_fixture(&original);
    let _migrated = migrate(&f).unwrap();
    assert!(matches!(
        original.commit_formation(pending),
        Err(StoreError::Conflict)
    ));
    assert!(original.load(NOW).unwrap().document().is_none());
}
#[test]
fn migration_verifies_expired_ownership_and_rejects_invalid_proofs_and_extra_fields() {
    let f = Fixture::new();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../team_state_support/proofs.json")).unwrap();
    let jwks = serde_json::to_vec(&fixture["jwks"]).unwrap();
    let policy = || {
        StorePolicy::new(
            f.profile.clone(),
            Some((
                ProofConfig {
                    issuer: "team-issuer".into(),
                    audience: "team-audience".into(),
                    token_type: "team-proof".into(),
                },
                &jwks,
            )),
        )
        .unwrap()
    };
    let store = FullTeamStore::create_new(
        &f.root,
        f.identity.clone(),
        policy(),
        Duration::from_secs(1),
    )
    .unwrap();
    let peer = SecretKey::from_bytes(&[2; 32]);
    let state = f.formation();
    let mut document = add(
        &state,
        f.identity.secret_key(),
        &peer,
        "alex",
        &state.document().link.as_ref().unwrap().body.secret,
    )
    .document()
    .clone();
    document.attestations.insert(
        peer.public().to_string(),
        fixture["tokens"]["expired_owner"].as_str().unwrap().into(),
    );
    seal(&store, document.clone());
    legacy::v1_fixture(&store);
    drop(store);
    let mut migrated = FullTeamStore::migrate_v1(
        &f.root,
        f.identity.clone(),
        policy(),
        Duration::from_secs(1),
        NOW,
    )
    .unwrap();
    assert!(migrated.load(NOW).unwrap().document().unwrap() == &document);
    document
        .attestations
        .insert(peer.public().to_string(), "bad-token".into());
    seal(&migrated, document);
    legacy::v1_fixture(&migrated);
    drop(migrated);
    assert!(matches!(
        FullTeamStore::migrate_v1(
            &f.root,
            f.identity.clone(),
            policy(),
            Duration::from_secs(1),
            NOW
        ),
        Err(StoreError::Corrupt)
    ));
    let connection = Connection::open(f.root.join(FULL_TEAM_DATABASE)).unwrap();
    let bytes = checkpoint::read_bytes(&connection, &f.identity, 1).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["extra"] = true.into();
    let bytes = serde_json::to_vec(&value).unwrap();
    let signature = f
        .identity
        .secret_key()
        .sign(&checkpoint::preimage(&bytes, 1));
    connection
        .execute(
            "UPDATE checkpoint SET payload=?1,signature=?2",
            rusqlite::params![bytes, signature.to_bytes().as_slice()],
        )
        .unwrap();
    assert!(matches!(
        FullTeamStore::migrate_v1(
            &f.root,
            f.identity.clone(),
            policy(),
            Duration::from_secs(1),
            NOW
        ),
        Err(StoreError::Corrupt)
    ));
}
