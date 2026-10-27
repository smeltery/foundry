#![cfg(unix)]
#[path = "full_team_store_support/mod.rs"]
mod support;
use foundry_network::full_team_store::{
    FULL_TEAM_DATABASE, FullTeamStore, MAX_CHECKPOINT_BYTES, StoreError,
};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    sync::{Arc, Barrier},
    time::{Duration, Instant},
};
use support::*;

#[test]
fn real_concurrent_formation_commits_one_candidate() {
    let f = Fixture::new();
    let mut one = f.create();
    let mut two = f.open();
    let base = one.load(NOW).unwrap();
    let first = one.prepare_formation(&base, formation(), NOW).unwrap();
    let base = two.load(NOW).unwrap();
    let second = two.prepare_formation(&base, formation(), NOW).unwrap();
    let start = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [(one, first), (two, second)]
        .into_iter()
        .map(|(mut store, pending)| {
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                store.commit_formation(pending).map(|s| s.revision())
            })
        })
        .collect();
    let outcomes: Vec<_> = workers.into_iter().map(|worker| worker.join()).collect();
    let outcomes: Vec<_> = outcomes.into_iter().map(Result::unwrap).collect();
    assert_eq!(outcomes.iter().filter(|r| **r == Ok(1)).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|r| **r == Err(StoreError::Conflict))
            .count(),
        1
    );
    assert_eq!(f.open().load(NOW).unwrap().revision(), 1);
}
#[test]
fn advisory_contention_is_bounded_and_does_not_mutate() {
    let f = Fixture::new();
    let mut store = f.create();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.base.root.join("full-team.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let start = Instant::now();
    assert!(matches!(store.load(NOW), Err(StoreError::Busy)));
    assert!(start.elapsed() < Duration::from_secs(2));
    lock.unlock().unwrap();
    assert!(store.load(NOW).unwrap().document().is_none());
}
#[test]
fn private_files_and_symlinks_are_checked_before_sqlite_access() {
    let f = Fixture::new();
    let mut store = f.create();
    let path = f.base.root.join(FULL_TEAM_DATABASE);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(store.load(NOW), Err(StoreError::UnsafeStorage)));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    drop(store);
    let original = f.base.root.join("held.sqlite3");
    fs::rename(&path, &original).unwrap();
    symlink(&original, &path).unwrap();
    assert!(
        FullTeamStore::open_existing(&f.base.root, f.identity.clone(), f.policy(), WAIT).is_err()
    );
    fs::remove_file(&path).unwrap();
    fs::rename(&original, &path).unwrap();
    symlink(&path, f.base.root.join(format!("{FULL_TEAM_DATABASE}-wal"))).unwrap();
    assert!(
        FullTeamStore::open_existing(&f.base.root, f.identity.clone(), f.policy(), WAIT).is_err()
    );
}
#[test]
fn replacement_inode_and_missing_checkpoint_are_not_empty_state() {
    let f = Fixture::new();
    let mut store = f.create();
    let base = store.load(NOW).unwrap();
    let candidate = store.prepare_formation(&base, formation(), NOW).unwrap();
    let path = f.base.root.join(FULL_TEAM_DATABASE);
    let old = f.base.root.join("old.sqlite3");
    fs::rename(&path, &old).unwrap();
    fs::copy(&old, &path).unwrap();
    assert!(matches!(
        store.commit_formation(candidate),
        Err(StoreError::UnsafeStorage)
    ));
    drop(store);
    let mut replacement = f.open();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute("DELETE FROM checkpoint", []).unwrap();
    assert!(matches!(replacement.load(NOW), Err(StoreError::Corrupt)));
    assert!(
        FullTeamStore::open_existing(&f.base.root, f.identity.clone(), f.policy(), WAIT).is_err()
    );
}
#[test]
fn schema_signature_and_payload_limits_fail_explicitly() {
    for kind in 0..3 {
        let f = Fixture::new();
        let mut store = f.create();
        let connection = rusqlite::Connection::open(f.base.root.join(FULL_TEAM_DATABASE)).unwrap();
        let expected = match kind {
            0 => {
                connection.execute_batch("PRAGMA user_version=99").unwrap();
                StoreError::Schema
            }
            1 => {
                connection
                    .execute("UPDATE checkpoint SET signature=zeroblob(64)", [])
                    .unwrap();
                StoreError::Corrupt
            }
            _ => {
                connection
                    .execute(
                        "UPDATE checkpoint SET payload=zeroblob(?1)",
                        [MAX_CHECKPOINT_BYTES + 1],
                    )
                    .unwrap();
                StoreError::TooLarge
            }
        };
        assert!(matches!(store.load(NOW), Err(error) if error == expected));
    }
}
