//! The credential file: owner-only permissions, its own identity and version,
//! and refusal of foreign or newer files before WAL is enabled.
#![cfg(unix)]

#[path = "credentials_support.rs"]
mod support;

use foundry_providers::credentials::{CredentialError, CredentialStore};
use rusqlite::Connection;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use support::*;

fn mode(file: &Path) -> u32 {
    std::fs::metadata(file).unwrap().permissions().mode() & 0o777
}

fn journal_mode(file: &Path) -> String {
    let connection = Connection::open(file).unwrap();
    connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap()
}

fn refused(file: &Path) -> CredentialError {
    CredentialStore::open(file)
        .err()
        .expect("open must be refused")
}

/// A file prepared by another program, left in rollback-journal mode.
fn prepared(directory: &Path, sql: &str) -> std::path::PathBuf {
    let file = path(directory);
    Connection::open(&file).unwrap().execute_batch(sql).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    file
}

fn assert_untouched(file: &Path, refusal: CredentialError) {
    let before = std::fs::read(file).unwrap();
    assert_eq!(refused(file), refusal);
    assert_eq!(std::fs::read(file).unwrap(), before);
    assert!(!Path::new(&format!("{}-wal", file.display())).exists());
    assert_eq!(journal_mode(file), "delete");
}

#[test]
fn new_file_is_owner_only_wal_with_its_own_identity() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path());
    store
        .configure(&owner(), &catalog(), request("fixture", None, CANARY, &[]))
        .unwrap();
    let file = path(directory.path());
    assert_eq!(mode(&file), 0o600);
    for suffix in ["-wal", "-shm"] {
        assert_eq!(
            mode(Path::new(&format!("{}{suffix}", file.display()))),
            0o600,
            "{suffix}"
        );
    }
    let inspect = Connection::open(&file).unwrap();
    let application_id: i64 = inspect
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .unwrap();
    assert_eq!(application_id, 0x4643_5244);
    let version: i64 = inspect
        .query_row("SELECT version FROM credential_schema_version", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(version, 1);
    assert_eq!(journal_mode(&file), "wal");
}

#[test]
fn shared_linked_or_special_files_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    let file = path(directory.path());
    std::fs::write(&file, b"").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
    let not_private = CredentialError::Unavailable {
        detail: "credential file is not private",
    };
    assert_eq!(refused(&file), not_private);

    let link = directory.path().join("linked.sqlite");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(refused(&link), not_private);
    assert_eq!(refused(directory.path()), not_private);
}

#[test]
fn foreign_databases_are_refused_untouched() {
    let directory = tempfile::tempdir().unwrap();
    let file = prepared(
        directory.path(),
        "CREATE TABLE other (value TEXT); INSERT INTO other VALUES ('x');",
    );
    assert_untouched(
        &file,
        CredentialError::Unavailable {
            detail: "not a credential database",
        },
    );

    std::fs::remove_file(&file).unwrap();
    let file = prepared(directory.path(), "PRAGMA application_id = 7;");
    assert_untouched(
        &file,
        CredentialError::Unavailable {
            detail: "not a credential database",
        },
    );

    std::fs::write(&file, b"plain text, not a database file at all.......................................................................").unwrap();
    let before = std::fs::read(&file).unwrap();
    assert_eq!(refused(&file).class(), "unavailable");
    assert_eq!(std::fs::read(&file).unwrap(), before);
}

#[test]
fn newer_or_unversioned_credential_files_are_refused_before_wal() {
    let directory = tempfile::tempdir().unwrap();
    let file = prepared(
        directory.path(),
        "PRAGMA application_id = 1178817092;
         CREATE TABLE credential_schema_version (version INTEGER NOT NULL);
         INSERT INTO credential_schema_version VALUES (2);",
    );
    let newer = CredentialError::Unavailable {
        detail: "credential database is from a newer version",
    };
    assert_untouched(&file, newer);

    Connection::open(&file)
        .unwrap()
        .execute_batch("DELETE FROM credential_schema_version;")
        .unwrap();
    assert_untouched(
        &file,
        CredentialError::Unavailable {
            detail: "unsupported credential schema",
        },
    );
}

#[test]
fn concurrent_openers_of_a_fresh_file_all_succeed() {
    let directory = tempfile::tempdir().unwrap();
    let file = path(directory.path());
    let openers: Vec<_> = (0..6)
        .map(|_| {
            let file = file.clone();
            std::thread::spawn(move || CredentialStore::open(&file).map(|_| ()))
        })
        .collect();
    for opener in openers {
        assert_eq!(opener.join().unwrap(), Ok(()));
    }
    assert_eq!(journal_mode(&file), "wal");
}

#[test]
fn shared_or_linked_sidecar_files_are_refused() {
    let not_private = CredentialError::Unavailable {
        detail: "credential file is not private",
    };
    for suffix in ["-wal", "-shm"] {
        let directory = tempfile::tempdir().unwrap();
        let file = path(directory.path());
        let sidecar = std::path::PathBuf::from(format!("{}{suffix}", file.display()));
        std::fs::write(&sidecar, b"").unwrap();
        std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(refused(&file), not_private, "{suffix}");

        std::fs::remove_file(&sidecar).unwrap();
        let elsewhere = directory.path().join("elsewhere");
        std::fs::write(&elsewhere, b"").unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &sidecar).unwrap();
        assert_eq!(refused(&file), not_private, "{suffix}");

        std::fs::remove_file(&sidecar).unwrap();
        assert!(CredentialStore::open(&file).is_ok(), "{suffix}");
    }
}
