#![cfg(unix)]
mod persistence_support;
use foundry_network::persistence::{MEMBERSHIP_DATABASE, MembershipStore, PersistenceError};
use persistence_support::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    time::Duration,
};

#[test]
fn database_and_sidecars_remain_private() {
    let fixture = Fixture::new();
    let mut store = fixture.initialized();
    store
        .merge(
            1,
            fixture.founder.public(),
            &fixture.update(2),
            &fixture.profile,
            NOW,
        )
        .unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let path = fixture.root.join(format!("{MEMBERSHIP_DATABASE}{suffix}"));
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
#[test]
fn symlink_nonregular_and_permissive_sidecars_are_rejected_before_open() {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        for kind in ["symlink", "directory", "mode", "hardlink", "fifo"] {
            let fixture = Fixture::new();
            let target = fixture.root.join(format!("{MEMBERSHIP_DATABASE}{suffix}"));
            let sentinel = fixture.root.join("sentinel");
            fs::write(&sentinel, b"unchanged").unwrap();
            fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o600)).unwrap();
            match kind {
                "symlink" => symlink(&sentinel, &target).unwrap(),
                "directory" => fs::create_dir(&target).unwrap(),
                "hardlink" => fs::hard_link(&sentinel, &target).unwrap(),
                "fifo" => {
                    use std::{ffi::CString, os::unix::ffi::OsStrExt};
                    let path = CString::new(target.as_os_str().as_bytes()).unwrap();
                    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
                }
                _ => {
                    fs::write(&target, b"").unwrap();
                    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
                }
            }
            assert!(
                matches!(
                    MembershipStore::open(&fixture.root, WAIT),
                    Err(PersistenceError::UnsafeStorage)
                ),
                "{suffix} {kind}"
            );
            assert_eq!(fs::read(&sentinel).unwrap(), b"unchanged");
        }
    }
}
#[test]
fn root_symlink_permissions_and_timeout_fail_closed() {
    let fixture = Fixture::new();
    for timeout in [Duration::ZERO, Duration::from_secs(61)] {
        assert!(matches!(
            MembershipStore::open(&fixture.root, timeout),
            Err(PersistenceError::Invalid)
        ));
    }
    let other = Fixture::new();
    let link = other.root.join("linked");
    symlink(&fixture.root, &link).unwrap();
    assert!(matches!(
        MembershipStore::open(&link, WAIT),
        Err(PersistenceError::UnsafeStorage)
    ));
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        MembershipStore::open(&fixture.root, WAIT),
        Err(PersistenceError::UnsafeStorage)
    ));
    assert!(!fixture.root.join(MEMBERSHIP_DATABASE).exists());
}
#[test]
fn changed_db_inode_and_postopen_sidecar_permissions_fail_closed() {
    let fixture = Fixture::new();
    let mut store = fixture.initialized();
    let wal = fixture.root.join(format!("{MEMBERSHIP_DATABASE}-wal"));
    fs::set_permissions(&wal, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        store.load("team", fixture.founder.public(), &fixture.profile, NOW),
        Err(PersistenceError::UnsafeStorage)
    ));
    fs::set_permissions(&wal, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(
        fixture.root.join(MEMBERSHIP_DATABASE),
        fixture.root.join("old"),
    )
    .unwrap();
    fs::write(fixture.root.join(MEMBERSHIP_DATABASE), b"").unwrap();
    fs::set_permissions(
        fixture.root.join(MEMBERSHIP_DATABASE),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(matches!(
        store.merge(
            1,
            fixture.founder.public(),
            &fixture.update(2),
            &fixture.profile,
            NOW
        ),
        Err(PersistenceError::UnsafeStorage)
    ));
}
