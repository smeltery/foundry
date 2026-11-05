//! The identity database file: no plaintext at rest or in Debug output,
//! durability settings, revocation across connections, and refusal of foreign
//! or newer databases.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use foundry_identity::authz::{PrincipalId, Scope, WorkspaceId};
use foundry_identity::store::{
    CredentialStore, IDENTITY_DATABASE, ManualClock, StoreError, SystemClock,
};
use rusqlite::Connection;

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::new(id)
}

fn who(id: &str) -> PrincipalId {
    PrincipalId::new(id)
}

fn open(path: &Path) -> Result<CredentialStore, StoreError> {
    CredentialStore::open(path, Arc::new(ManualClock::default()))
}

fn database() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(IDENTITY_DATABASE);
    (directory, path)
}

/// Every byte of the database and its journal files that exist now.
fn bytes_on_disk(path: &Path) -> Vec<u8> {
    ["", "-wal", "-shm", "-journal"]
        .iter()
        .filter_map(|suffix| std::fs::read(format!("{}{suffix}", path.display())).ok())
        .flatten()
        .collect()
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// The part of presented text after its public prefix.
fn secret_part(presented: &str) -> &str {
    presented.rsplit_once(['_', '-']).unwrap().1
}

#[test]
fn no_plaintext_secret_reaches_disk_debug_output_or_errors() {
    let (_directory, path) = database();
    let store = open(&path).unwrap();
    let key = store
        .issue_key(&who("svc-a"), &ws("ws-a"), [Scope::WorkspaceRead])
        .unwrap();
    let login = store
        .issue_login(&who("user-a"), &ws("ws-a"), [], Duration::from_secs(60))
        .unwrap();
    let pairing = store
        .issue_pairing(&who("user-a"), &ws("ws-a"), Duration::from_secs(60), 3)
        .unwrap();
    let presented = [
        key.secret.expose(),
        login.secret.expose(),
        pairing.code.expose(),
    ];
    let principal = store.authenticate(key.secret.expose()).unwrap();
    store.revoke_key(&ws("ws-a"), &key.record.prefix).unwrap();
    let refused = store.authenticate(key.secret.expose()).unwrap_err();

    let debug = format!(
        "{key:?} {login:?} {pairing:?} {principal:?} {refused:?} {refused} {store:?} {:?}",
        store.list_keys(&ws("ws-a")).unwrap()
    );
    assert!(debug.contains("SecretText(<redacted>)"));
    assert!(debug.contains(&key.record.prefix));
    let open_bytes = bytes_on_disk(&path);
    drop(store);
    let closed_bytes = bytes_on_disk(&path);
    // The public prefixes are stored, so the scan does see row contents.
    assert!(contains(&closed_bytes, &key.record.prefix));
    assert!(contains(&closed_bytes, &pairing.record.prefix));
    for text in presented {
        let secret = secret_part(text);
        assert!(!debug.contains(secret));
        assert!(!contains(&open_bytes, secret));
        assert!(!contains(&closed_bytes, secret));
    }
}

#[test]
fn file_uses_wal_with_its_own_application_id_and_version() {
    let (_directory, path) = database();
    let _store = open(&path).unwrap();
    let raw = Connection::open(&path).unwrap();
    let mode: String = raw
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    let application_id: i64 = raw
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .unwrap();
    let version: i64 = raw
        .query_row("SELECT version FROM identity_schema_version", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        (mode.as_str(), application_id, version),
        ("wal", 0x4649_444E, 1)
    );
}

#[cfg(unix)]
#[test]
fn a_new_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let _store = CredentialStore::open_in(directory.path(), Arc::new(SystemClock)).unwrap();
    let mode = std::fs::metadata(directory.path().join(IDENTITY_DATABASE))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn revocation_on_one_connection_applies_to_the_next_request_on_another() {
    let (_directory, path) = database();
    let issuer = open(&path).unwrap();
    let verifier = open(&path).unwrap();
    let key = issuer.issue_key(&who("svc-a"), &ws("ws-a"), []).unwrap();
    assert!(verifier.authenticate(key.secret.expose()).is_ok());
    issuer.revoke_key(&ws("ws-a"), &key.record.prefix).unwrap();
    assert_eq!(
        verifier.authenticate(key.secret.expose()),
        Err(StoreError::Unauthenticated)
    );
    drop((issuer, verifier));
    assert_eq!(
        open(&path).unwrap().authenticate(key.secret.expose()),
        Err(StoreError::Unauthenticated)
    );
}

#[test]
fn concurrent_first_opens_of_a_new_file_all_succeed() {
    let (_directory, path) = database();
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (barrier, path) = (barrier.clone(), path.clone());
            thread::spawn(move || {
                barrier.wait();
                open(&path).map(|_| ())
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
}

fn assert_refused(path: &Path) {
    let refused = open(path).unwrap_err();
    assert_eq!(refused.class(), "unavailable");
}

#[test]
fn a_foreign_database_is_refused_untouched() {
    let (_directory, path) = database();
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch("CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('kept');")
        .unwrap();
    assert_refused(&path);
    let body: String = raw
        .query_row("SELECT body FROM notes", [], |row| row.get(0))
        .unwrap();
    let application_id: i64 = raw
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .unwrap();
    assert_eq!((body.as_str(), application_id), ("kept", 0));
}

#[test]
fn another_application_id_is_refused() {
    let (_directory, path) = database();
    Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA application_id = 7;")
        .unwrap();
    assert_refused(&path);
}

#[test]
fn a_newer_or_missing_schema_version_is_refused() {
    for statement in [
        "UPDATE identity_schema_version SET version = 2",
        "DELETE FROM identity_schema_version",
    ] {
        let (_directory, path) = database();
        drop(open(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .execute_batch(statement)
            .unwrap();
        assert_refused(&path);
    }
}
