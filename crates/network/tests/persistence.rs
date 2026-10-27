#![cfg(unix)]
mod persistence_support;
use foundry_network::persistence::{MembershipStore, PersistenceError};
use iroh::SecretKey;
use persistence_support::*;
use std::{
    sync::{Arc, Barrier},
    thread,
};

#[test]
fn initialize_is_insert_only_and_noop_preserves_revision() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    assert!(
        store
            .load("team", fixture.founder.public(), &fixture.profile, NOW)
            .unwrap()
            .is_none()
    );
    let snapshot = fixture.initial();
    let state = fixture.state(&snapshot);
    assert_eq!(
        store
            .initialize(&state, &fixture.profile, NOW)
            .unwrap()
            .revision,
        1
    );
    assert!(matches!(
        store.initialize(&state, &fixture.profile, NOW),
        Err(PersistenceError::AlreadyInitialized)
    ));
    let (stored, report) = store
        .merge(
            1,
            fixture.founder.public(),
            &snapshot,
            &fixture.profile,
            NOW,
        )
        .unwrap();
    assert_eq!(stored.revision, 1);
    assert!(!report.changed);
    drop(store);
    assert_eq!(
        fixture
            .open()
            .load("team", fixture.founder.public(), &fixture.profile, NOW)
            .unwrap()
            .unwrap()
            .state
            .snapshot(),
        &snapshot
    );
}
#[test]
fn removal_survives_reopen_and_stale_member_replay() {
    let fixture = Fixture::new();
    let mut store = fixture.initialized();
    let peer = SecretKey::generate();
    let mut added = fixture.initial();
    let member = fixture.member(&peer, true, 2);
    added
        .admissions
        .insert(peer.public().to_string(), member.clone());
    added.members.insert(peer.public().to_string(), member);
    store
        .merge(1, fixture.founder.public(), &added, &fixture.profile, NOW)
        .unwrap();
    let mut removed = added.clone();
    removed
        .members
        .insert(peer.public().to_string(), fixture.member(&peer, false, 3));
    store
        .merge(2, fixture.founder.public(), &removed, &fixture.profile, NOW)
        .unwrap();
    drop(store);
    let mut store = fixture.open();
    assert!(
        !store
            .load("team", fixture.founder.public(), &fixture.profile, NOW)
            .unwrap()
            .unwrap()
            .state
            .is_active(peer.public())
    );
    let (stored, report) = store
        .merge(3, fixture.founder.public(), &added, &fixture.profile, NOW)
        .unwrap();
    assert!(!stored.state.is_active(peer.public()));
    assert_eq!(stored.revision, 3);
    assert!(!report.changed);
    assert!(
        store
            .merge(3, peer.public(), &removed, &fixture.profile, NOW)
            .is_err()
    );
}
#[test]
fn concurrent_writers_have_exactly_one_cas_winner() {
    let fixture = Fixture::new();
    drop(fixture.initialized());
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = [2, 3]
        .into_iter()
        .map(|ts| {
            let root = fixture.root.clone();
            let profile = fixture.profile.clone();
            let update = fixture.update(ts);
            let peer = fixture.founder.public();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let mut store = MembershipStore::open(&root, WAIT).unwrap();
                barrier.wait();
                store
                    .merge(1, peer, &update, &profile, NOW)
                    .map(|(s, _)| s.revision)
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| matches!(r, Ok(2))).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(PersistenceError::Conflict)))
            .count(),
        1
    );
}
#[test]
fn failure_rolls_back_and_busy_has_bounded_timeout() {
    let fixture = Fixture::new();
    let mut store = fixture.initialized();
    let connection = fixture.connection();
    connection.execute_batch("CREATE TRIGGER reject_update BEFORE UPDATE ON projection BEGIN SELECT RAISE(ABORT,'fixture'); END").unwrap();
    assert!(matches!(
        store.merge(
            1,
            fixture.founder.public(),
            &fixture.update(2),
            &fixture.profile,
            NOW
        ),
        Err(PersistenceError::Storage)
    ));
    assert_eq!(
        store
            .load("team", fixture.founder.public(), &fixture.profile, NOW)
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    connection
        .execute_batch("DROP TRIGGER reject_update; BEGIN IMMEDIATE")
        .unwrap();
    let began = std::time::Instant::now();
    assert!(matches!(
        store.merge(
            1,
            fixture.founder.public(),
            &fixture.update(2),
            &fixture.profile,
            NOW
        ),
        Err(PersistenceError::Busy)
    ));
    assert!(began.elapsed() < std::time::Duration::from_secs(2));
    connection.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        store
            .merge(
                1,
                fixture.founder.public(),
                &fixture.update(2),
                &fixture.profile,
                NOW
            )
            .unwrap()
            .0
            .revision,
        2
    );
}
#[test]
fn revision_overflow_preserves_stored_state_and_noop_still_works() {
    let fixture = Fixture::new();
    let mut store = fixture.initialized();
    fixture
        .connection()
        .execute("UPDATE projection SET revision=?1", [i64::MAX])
        .unwrap();
    assert!(matches!(
        store.merge(
            i64::MAX as u64,
            fixture.founder.public(),
            &fixture.update(2),
            &fixture.profile,
            NOW
        ),
        Err(PersistenceError::RevisionOverflow)
    ));
    assert_eq!(
        store
            .merge(
                i64::MAX as u64,
                fixture.founder.public(),
                &fixture.initial(),
                &fixture.profile,
                NOW
            )
            .unwrap()
            .0
            .revision,
        i64::MAX as u64
    );
}
#[test]
fn rejected_new_records_do_not_mutate_durable_snapshot() {
    let fixture = Fixture::new();
    let mut store = fixture.initialized();
    let mut incoming = fixture.update(2);
    incoming.metadata.sig = "00".repeat(64);
    let (stored, report) = store
        .merge(
            1,
            fixture.founder.public(),
            &incoming,
            &fixture.profile,
            NOW,
        )
        .unwrap();
    assert_eq!(report.rejected, 1);
    assert!(!report.changed);
    assert_eq!(stored.revision, 1);
    assert_eq!(stored.state.snapshot(), &fixture.initial());
}
