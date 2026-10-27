#![cfg(unix)]
#[path = "full_team_store_support/mod.rs"]
mod support;
use foundry_network::full_team_store::{
    FULL_TEAM_DATABASE, FullTeamStore, StoreError, StorePolicy,
};
use support::*;

#[test]
fn formation_reopen_is_exact_and_empty_load_never_forms_implicitly() {
    let f = Fixture::new();
    let mut store = f.create();
    let base = store.load(NOW).unwrap();
    assert_eq!(base.revision(), 0);
    assert!(base.document().is_none());
    let pending = store.prepare_formation(&base, formation(), NOW).unwrap();
    assert!(store.load(NOW).unwrap().document().is_none());
    let committed = store.commit_formation(pending).unwrap();
    assert_eq!(committed.revision(), 1);
    let expected = serde_json::to_vec(committed.document().unwrap()).unwrap();
    drop(store);
    let mut reopened = f.open();
    let loaded = reopened.load(NOW - 1).unwrap();
    assert_eq!(
        expected,
        serde_json::to_vec(loaded.document().unwrap()).unwrap()
    );
    assert!(matches!(
        reopened.prepare_formation(&loaded, formation(), NOW),
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        reopened.prepare_code_intent(),
        Err(StoreError::Unsupported)
    ));
    assert_eq!(reopened.load(NOW).unwrap().revision(), 1);
}
#[test]
fn foreign_instance_rejects_candidate_even_with_identical_checkpoint_and_identity() {
    let a = Fixture::new();
    let b = Fixture::new();
    let mut origin = a.create();
    let base = origin.load(NOW).unwrap();
    let pending = origin.prepare_formation(&base, formation(), NOW).unwrap();
    // Copy an identical sealed empty database, not merely a matching revision.
    let path_a = a.base.root.join(FULL_TEAM_DATABASE);
    let external = rusqlite::Connection::open(&path_a).unwrap();
    external
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(external);
    std::fs::copy(&path_a, b.base.root.join(FULL_TEAM_DATABASE)).unwrap();
    let mut other =
        FullTeamStore::open_existing(&b.base.root, a.identity.clone(), a.policy(), WAIT).unwrap();
    assert_eq!(other.load(NOW).unwrap().revision(), base.revision());
    assert!(matches!(
        other.commit_formation(pending),
        Err(StoreError::Binding)
    ));
    assert!(other.load(NOW).unwrap().document().is_none());
}
#[test]
fn reopen_rejects_old_capabilities_and_two_handles_observe_cas_conflict() {
    let f = Fixture::new();
    let mut first = f.create();
    let base = first.load(NOW).unwrap();
    let stale = first.prepare_formation(&base, formation(), NOW).unwrap();
    drop(first);
    let mut reopened = f.open();
    assert!(matches!(
        reopened.commit_formation(stale),
        Err(StoreError::Binding)
    ));
    assert!(matches!(
        reopened.prepare_formation(&base, formation(), NOW),
        Err(StoreError::Binding)
    ));
    let base = reopened.load(NOW).unwrap();
    let one = reopened.prepare_formation(&base, formation(), NOW).unwrap();
    let mut another = f.open();
    let base = another.load(NOW).unwrap();
    let two = another.prepare_formation(&base, formation(), NOW).unwrap();
    reopened.commit_formation(one).unwrap();
    assert!(matches!(
        another.commit_formation(two),
        Err(StoreError::Conflict)
    ));
    assert_eq!(another.load(NOW).unwrap().revision(), 1);
}
#[test]
fn deleted_database_rejects_candidate_without_recreation() {
    let f = Fixture::new();
    let mut store = f.create();
    let base = store.load(NOW).unwrap();
    let pending = store.prepare_formation(&base, formation(), NOW).unwrap();
    let path = f.base.root.join(FULL_TEAM_DATABASE);
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
        store.commit_formation(pending),
        Err(StoreError::UnsafeStorage)
    ));
    assert!(!path.exists());
    assert!(
        FullTeamStore::open_existing(&f.base.root, f.identity.clone(), f.policy(), WAIT).is_err()
    );
    assert!(!path.exists());
}
#[test]
fn policy_identity_and_existing_database_are_never_overwritten() {
    let f = Fixture::new();
    let store = f.create();
    drop(store);
    assert!(FullTeamStore::create_new(&f.base.root, f.identity.clone(), f.policy(), WAIT).is_err());
    let other = Fixture::new();
    assert!(
        FullTeamStore::open_existing(&f.base.root, other.identity.clone(), f.policy(), WAIT)
            .is_err()
    );
    let mut profile = f.base.profile.clone();
    profile.protocol += 1;
    let policy = StorePolicy::new(profile, None).unwrap();
    assert!(matches!(
        FullTeamStore::open_existing(&f.base.root, f.identity.clone(), policy, WAIT),
        Err(StoreError::Binding)
    ));
    assert!(f.open().load(NOW).unwrap().document().is_none());
}
#[test]
fn oversized_candidate_does_not_change_empty_checkpoint() {
    let f = Fixture::new();
    let mut store = f.create();
    let base = store.load(NOW).unwrap();
    let name = "x".repeat(foundry_network::full_team_store::MAX_CHECKPOINT_BYTES);
    let fields = foundry_network::team_state::Formation {
        name: &name,
        person: "alice",
        machine: "desk",
    };
    assert!(matches!(
        store.prepare_formation(&base, fields, NOW),
        Err(StoreError::TooLarge)
    ));
    assert!(store.load(NOW).unwrap().document().is_none());
}
