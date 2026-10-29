#![cfg(unix)]
use cap_std::{ambient_authority, fs::Dir};
use foundry_network::daemon_lease::{DaemonLease, LeaseError};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
};

struct State(PathBuf);
impl State {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "daemon-lease-{}",
            iroh::SecretKey::generate().public()
        ));
        fs::DirBuilder::new().create(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn dir(&self) -> Dir {
        Dir::open_ambient_dir(&self.0, ambient_authority()).unwrap()
    }
}
impl Drop for State {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn exclusive_ownership_releases_without_unlinking() {
    let state = State::new();
    let root = state.dir();
    let lease = DaemonLease::acquire(&root).unwrap();
    assert!(matches!(
        DaemonLease::acquire(&root),
        Err(LeaseError::AlreadyRunning)
    ));
    let metadata = fs::metadata(state.0.join("daemon.lock")).unwrap();
    drop(lease);
    let _next = DaemonLease::acquire(&root).unwrap();
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        metadata.ino(),
        fs::metadata(state.0.join("daemon.lock")).unwrap().ino()
    );
}

#[test]
fn shared_directories_symlinks_and_hardlinks_are_rejected() {
    let state = State::new();
    let root = state.dir();
    fs::set_permissions(&state.0, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        DaemonLease::acquire(&root),
        Err(LeaseError::UnsafeStorage)
    ));
    assert!(!state.0.join("daemon.lock").exists());
    fs::set_permissions(&state.0, fs::Permissions::from_mode(0o700)).unwrap();
    let target = state.0.join("untouched");
    fs::write(&target, b"sentinel").unwrap();
    symlink(&target, state.0.join("daemon.lock")).unwrap();
    assert!(matches!(
        DaemonLease::acquire(&root),
        Err(LeaseError::UnsafeStorage)
    ));
    assert_eq!(fs::read(&target).unwrap(), b"sentinel");
    fs::remove_file(state.0.join("daemon.lock")).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&target, state.0.join("daemon.lock")).unwrap();
    assert!(matches!(
        DaemonLease::acquire(&root),
        Err(LeaseError::UnsafeStorage)
    ));
}
