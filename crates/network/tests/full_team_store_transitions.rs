#![cfg(unix)]
#[path = "full_team_store_support/mod.rs"]
mod support;
use foundry_network::{
    full_team_store::{CommitOutcome, LoadedStatus, LoadedTeam, ScrubStatus, StoreError},
    team_state::Admission,
};
use iroh::{EndpointId, SecretKey};
use support::*;
fn active(outcome: CommitOutcome) -> LoadedTeam {
    match outcome {
        CommitOutcome::Active(state) | CommitOutcome::Unchanged(state) => state,
        _ => panic!("expected active state"),
    }
}
fn formed(f: &Fixture) -> (foundry_network::full_team_store::FullTeamStore, LoadedTeam) {
    let mut store = f.create();
    let base = store.load(NOW).unwrap();
    let pending = store.prepare_formation(&base, formation(), NOW).unwrap();
    let state = store.commit_formation(pending).unwrap();
    (store, state)
}
fn request<'a>(endpoint: EndpointId, secret: &'a str, person: &'a str) -> Admission<'a> {
    Admission {
        endpoint,
        secret,
        person,
        machine: "desk",
        proof: None,
        owner_only: false,
    }
}
#[test]
fn typed_invite_admit_rotate_remove_are_durable_and_noop_is_cas() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let pending = store.prepare_invite(&base, "one", 1, false, NOW).unwrap();
    let invited = active(store.commit_transition(pending, NOW).unwrap());
    let peer = SecretKey::from_bytes(&[2; 32]);
    let pending = store
        .prepare_admission(&invited, request(peer.public(), "one", "bob"), NOW)
        .unwrap();
    let admitted = active(store.commit_transition(pending, NOW).unwrap());
    assert!(
        admitted.document().unwrap().members[&peer.public().to_string()]
            .body
            .active
    );
    let (noop, report) = store
        .prepare_merge(
            &admitted,
            f.identity.endpoint_id(),
            admitted.document().unwrap(),
            NOW,
        )
        .unwrap();
    assert!(!report.changed && !noop.requires_quiescence());
    let same = active(store.commit_transition(noop, NOW).unwrap());
    assert_eq!(same.revision(), admitted.revision());
    let (stale, _) = store
        .prepare_merge(
            &same,
            f.identity.endpoint_id(),
            same.document().unwrap(),
            NOW,
        )
        .unwrap();
    let rotation = store.prepare_rotation(&same, NOW + 1).unwrap();
    let rotated = active(store.commit_transition(rotation, NOW + 1).unwrap());
    assert!(matches!(
        store.commit_transition(stale, NOW + 1),
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        store.prepare_remove(&rotated, &vec![peer.public(); 4097], NOW + 2),
        Err(StoreError::TooLarge)
    ));
    let pending = store
        .prepare_remove(&rotated, &[peer.public()], NOW + 2)
        .unwrap();
    let removed = active(store.commit_transition(pending, NOW + 2).unwrap());
    assert!(
        !removed.document().unwrap().members[&peer.public().to_string()]
            .body
            .active
    );
    assert!(removed.document().unwrap().link != rotated.document().unwrap().link);
    drop(store);
    assert!(f.open().load(NOW + 2).unwrap().document() == removed.document());
}
#[test]
fn departure_tombstone_never_becomes_empty_and_fences_stale_candidates() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let stale = store.prepare_rotation(&base, NOW + 1).unwrap();
    let (stale_noop, _) = store
        .prepare_merge(
            &base,
            f.identity.endpoint_id(),
            base.document().unwrap(),
            NOW,
        )
        .unwrap();
    let pending = store.prepare_departure(&base, NOW + 1).unwrap();
    assert!(pending.requires_quiescence());
    let receipt = match store.commit_transition(pending, NOW + 1).unwrap() {
        CommitOutcome::Removed { receipt, scrub } => {
            assert_eq!(scrub, ScrubStatus::Complete);
            receipt
        }
        _ => panic!("expected removal"),
    };
    assert!(matches!(
        store.commit_transition(stale, NOW + 1),
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        store.commit_transition(stale_noop, NOW + 1),
        Err(StoreError::Conflict)
    ));
    assert_eq!(store.retry_scrub(&receipt).unwrap(), ScrubStatus::Complete);
    assert!(store.load(NOW + 1).is_err());
    drop(store);
    let mut reopened = f.open();
    assert!(matches!(
        reopened.retry_scrub(&receipt),
        Err(StoreError::Binding)
    ));
    let receipt = match reopened.load_status(NOW + 1).unwrap() {
        LoadedStatus::Removed(receipt) => receipt,
        _ => panic!("tombstone missing"),
    };
    assert_eq!(
        reopened.retry_scrub(&receipt).unwrap(),
        ScrubStatus::Complete
    );
    assert!(
        reopened
            .prepare_formation(&base, formation(), NOW + 1)
            .is_err()
    );
}
#[test]
fn held_reader_reports_pending_scrub_and_retry_does_not_write_complete_flag() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let reader = rusqlite::Connection::open(
        f.base
            .root
            .join(foundry_network::full_team_store::FULL_TEAM_DATABASE),
    )
    .unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let _: Vec<u8> = reader
        .query_row("SELECT payload FROM checkpoint", [], |r| r.get(0))
        .unwrap();
    let pending = store.prepare_departure(&base, NOW + 1).unwrap();
    let receipt = match store.commit_transition(pending, NOW + 1).unwrap() {
        CommitOutcome::Removed { receipt, scrub } => {
            assert_eq!(scrub, ScrubStatus::PendingBusy);
            receipt
        }
        _ => panic!("expected removal"),
    };
    reader.execute_batch("ROLLBACK").unwrap();
    assert_eq!(store.retry_scrub(&receipt).unwrap(), ScrubStatus::Complete);
    let revision = receipt.revision();
    assert_eq!(store.retry_scrub(&receipt).unwrap(), ScrubStatus::Complete);
    match store.load_status(NOW + 1).unwrap() {
        LoadedStatus::Removed(receipt) => assert_eq!(receipt.revision(), revision),
        _ => panic!("expected removed"),
    }
    let wal = f.base.root.join("full-team.sqlite3-wal");
    assert!(std::fs::metadata(wal).map_or(true, |m| m.len() == 0));
}
#[test]
fn removal_disposition_is_latched_before_conflict_or_protection_failure() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let removal = store.prepare_departure(&base, NOW + 1).unwrap();
    let quiesce = removal.requires_quiescence();
    let rotation = store.prepare_rotation(&base, NOW + 1).unwrap();
    let newer = active(store.commit_transition(rotation, NOW + 1).unwrap());
    assert!(matches!(
        store.commit_transition(removal, NOW + 1),
        Err(StoreError::Conflict)
    ));
    assert!(quiesce);
    let removal = store.prepare_departure(&newer, NOW + 2).unwrap();
    let quiesce = removal.requires_quiescence();
    std::fs::remove_file(
        f.base
            .root
            .join(foundry_network::full_team_store::FULL_TEAM_DATABASE),
    )
    .unwrap();
    assert!(matches!(
        store.commit_transition(removal, NOW + 2),
        Err(StoreError::UnsafeStorage)
    ));
    assert!(quiesce);
}
#[test]
fn admission_uses_prepare_expiry_policy_without_replaying_at_commit() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let pending = store.prepare_invite(&base, "short", 1, false, NOW).unwrap();
    let base = active(store.commit_transition(pending, NOW).unwrap());
    let pending = store
        .prepare_admission(
            &base,
            request(SecretKey::from_bytes(&[2; 32]).public(), "short", "bob"),
            NOW + 1,
        )
        .unwrap();
    let committed = active(store.commit_transition(pending, NOW + 4_000_000).unwrap());
    assert!(
        committed.document().unwrap().members
            [&SecretKey::from_bytes(&[2; 32]).public().to_string()]
            .body
            .active
    );
}
