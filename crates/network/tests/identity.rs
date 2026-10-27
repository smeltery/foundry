#![cfg(unix)]
use cap_std::fs::Dir;
use foundry_network::{IdentityError, load_or_create_identity};
use iroh::SecretKey;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

const WAIT: Duration = Duration::from_secs(3);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("identity-test-{}", SecretKey::generate().public()));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn dir(&self) -> Dir {
        Dir::open_ambient_dir(&self.0, cap_std::ambient_authority()).unwrap()
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        fs::write(self.0.join(name), bytes).unwrap();
        fs::set_permissions(self.0.join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn child(&self, mode: &str, index: usize) -> ChildGuard {
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "identity_child", "--ignored"])
            .env("IDENTITY_TEST_ROOT", &self.0)
            .env("IDENTITY_TEST_MODE", mode)
            .env("IDENTITY_TEST_INDEX", index.to_string())
            .spawn()
            .unwrap();
        ChildGuard {
            child,
            reaped: false,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn wait(path: &Path) {
    let deadline = Instant::now() + WAIT;
    while !path.exists() {
        assert!(Instant::now() < deadline, "fixture deadline exceeded");
        thread::sleep(Duration::from_millis(5));
    }
}
struct ChildGuard {
    child: Child,
    reaped: bool,
}
impl ChildGuard {
    fn terminate(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            self.child.wait().unwrap();
            self.reaped = true;
        }
    }
    fn reap(&mut self) -> bool {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                self.reaped = true;
                return status.success();
            }
            if Instant::now() >= deadline {
                self.terminate();
                return false;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
#[ignore = "invoked as a bounded subprocess by identity tests"]
fn identity_child() {
    let root = PathBuf::from(std::env::var_os("IDENTITY_TEST_ROOT").unwrap());
    match std::env::var("IDENTITY_TEST_MODE").unwrap().as_str() {
        "create" => {
            wait(&root.join("go"));
            let dir = Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
            let identity = load_or_create_identity(&dir, WAIT).unwrap();
            let index = std::env::var("IDENTITY_TEST_INDEX").unwrap();
            fs::write(
                root.join(format!("result-{index}")),
                identity.endpoint_id().to_string(),
            )
            .unwrap();
        }
        "after-link" => {
            let key = SecretKey::generate();
            let temporary = root.join(format!(".identity-{}.tmp", key.public()));
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .unwrap();
            file.write_all(&key.to_bytes()).unwrap();
            file.sync_all().unwrap();
            fs::hard_link(&temporary, root.join("machine.key")).unwrap();
            fs::write(root.join("expected"), key.public().to_string()).unwrap();
            fs::write(root.join("linked"), b"").unwrap();
            wait(&root.join("never-release"));
        }
        "hold" => {
            let lock = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(root.join("identity.lock"))
                .unwrap();
            lock.lock().unwrap();
            fs::write(root.join("locked"), b"").unwrap();
            wait(&root.join("release"));
        }
        _ => panic!("invalid child mode"),
    }
}

#[test]
fn reopening_preserves_exact_raw_key_and_signatures() {
    let fixture = Fixture::new();
    let first = load_or_create_identity(&fixture.dir(), WAIT).unwrap();
    let bytes = fs::read(fixture.0.join("machine.key")).unwrap();
    assert_eq!(bytes.len(), 32);
    assert_eq!(bytes, first.secret_key().to_bytes());
    let second = load_or_create_identity(&fixture.dir(), WAIT).unwrap();
    assert_eq!(first.endpoint_id(), second.endpoint_id());
    second
        .endpoint_id()
        .verify(
            b"identity fixture",
            &first.secret_key().sign(b"identity fixture"),
        )
        .unwrap();
    for name in ["machine.key", "identity.lock"] {
        assert_eq!(
            fs::metadata(fixture.0.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 2);
}

#[test]
fn simultaneous_processes_publish_one_identity() {
    let fixture = Fixture::new();
    let mut children: Vec<_> = (0..6).map(|index| fixture.child("create", index)).collect();
    fs::write(fixture.0.join("go"), b"").unwrap();
    let statuses: Vec<_> = children.iter_mut().map(ChildGuard::reap).collect();
    assert!(
        statuses.iter().all(|success| *success),
        "child statuses: {statuses:?}"
    );
    let id = load_or_create_identity(&fixture.dir(), WAIT)
        .unwrap()
        .endpoint_id()
        .to_string();
    for index in 0..6 {
        assert_eq!(
            fs::read_to_string(fixture.0.join(format!("result-{index}"))).unwrap(),
            id
        );
    }
    assert!(!fs::read_dir(&fixture.0).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
}

#[test]
fn held_process_lock_times_out_then_recovers_without_rotation() {
    let fixture = Fixture::new();
    let original = load_or_create_identity(&fixture.dir(), WAIT)
        .unwrap()
        .endpoint_id();
    let mut child = fixture.child("hold", 0);
    wait(&fixture.0.join("locked"));
    let start = Instant::now();
    let result = load_or_create_identity(&fixture.dir(), Duration::from_millis(35));
    fs::write(fixture.0.join("release"), b"").unwrap();
    assert!(child.reap());
    assert!(matches!(result, Err(IdentityError::LockTimeout)));
    assert!(start.elapsed() < WAIT);
    assert_eq!(
        load_or_create_identity(&fixture.dir(), WAIT)
            .unwrap()
            .endpoint_id(),
        original
    );
}

#[test]
fn malformed_key_is_never_replaced() {
    for length in [0, 1, 31, 33, 4096] {
        let fixture = Fixture::new();
        let bytes = vec![b'x'; length];
        fixture.write("machine.key", &bytes);
        assert!(matches!(
            load_or_create_identity(&fixture.dir(), WAIT),
            Err(IdentityError::CorruptKey)
        ));
        assert_eq!(fs::read(fixture.0.join("machine.key")).unwrap(), bytes);
    }
}

#[test]
fn unsafe_modes_fail_without_repair_or_regeneration() {
    for target in ["", "machine.key", "identity.lock"] {
        for mode in [0o644, 0o000, 0o660, 0o1600] {
            let fixture = Fixture::new();
            fixture.write("machine.key", &[7; 32]);
            fixture.write("identity.lock", b"");
            fs::set_permissions(fixture.0.join(target), fs::Permissions::from_mode(mode)).unwrap();
            let dir = if target.is_empty() {
                // Obtain capability before mode checks using an already opened handle.
                fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
                let dir = fixture.dir();
                fs::set_permissions(&fixture.0, fs::Permissions::from_mode(mode)).unwrap();
                dir
            } else {
                fixture.dir()
            };
            assert!(matches!(
                load_or_create_identity(&dir, WAIT),
                Err(IdentityError::UnsafeStorage)
            ));
            fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(
                fixture.0.join("machine.key"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            assert_eq!(fs::read(fixture.0.join("machine.key")).unwrap(), [7; 32]);
        }
    }
}

#[test]
fn symlink_and_directory_targets_are_rejected() {
    for target in ["machine.key", "identity.lock"] {
        for directory in [false, true] {
            let fixture = Fixture::new();
            fixture.write("sentinel", &[9; 32]);
            if directory {
                fs::create_dir(fixture.0.join(target)).unwrap();
            } else {
                symlink("sentinel", fixture.0.join(target)).unwrap();
            }
            assert!(matches!(
                load_or_create_identity(&fixture.dir(), WAIT),
                Err(IdentityError::UnsafeStorage)
            ));
            assert_eq!(fs::read(fixture.0.join("sentinel")).unwrap(), [9; 32]);
            assert_eq!(
                fs::symlink_metadata(fixture.0.join(target))
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                !directory
            );
        }
    }
}

#[test]
fn fifo_and_hardlinks_are_rejected_without_blocking() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    for target in ["machine.key", "identity.lock"] {
        let fixture = Fixture::new();
        let path = CString::new(fixture.0.join(target).as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(matches!(
            load_or_create_identity(&fixture.dir(), WAIT),
            Err(IdentityError::UnsafeStorage)
        ));
        fs::remove_file(fixture.0.join(target)).unwrap();
        fixture.write("sentinel", &[8; 32]);
        fs::hard_link(fixture.0.join("sentinel"), fixture.0.join(target)).unwrap();
        assert!(matches!(
            load_or_create_identity(&fixture.dir(), WAIT),
            Err(IdentityError::UnsafeStorage)
        ));
        assert_eq!(fs::read(fixture.0.join("sentinel")).unwrap(), [8; 32]);
    }
}

#[test]
fn invalid_timeout_does_not_touch_directory() {
    let fixture = Fixture::new();
    for timeout in [Duration::ZERO, Duration::from_secs(61), Duration::MAX] {
        assert!(matches!(
            load_or_create_identity(&fixture.dir(), timeout),
            Err(IdentityError::InvalidTimeout)
        ));
    }
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
    assert_eq!(
        IdentityError::CorruptKey.to_string(),
        "invalid stored identity"
    );
}

#[test]
fn killed_publisher_after_link_recovers_exact_identity() {
    let fixture = Fixture::new();
    let mut child = fixture.child("after-link", 0);
    wait(&fixture.0.join("linked"));
    child.terminate();
    let expected = fs::read_to_string(fixture.0.join("expected")).unwrap();
    let bytes = fs::read(fixture.0.join("machine.key")).unwrap();
    let temporary = fixture.0.join(format!(".identity-{expected}.tmp"));
    assert!(temporary.exists());
    let identity = load_or_create_identity(&fixture.dir(), WAIT).unwrap();
    assert_eq!(identity.endpoint_id().to_string(), expected);
    assert_eq!(fs::read(fixture.0.join("machine.key")).unwrap(), bytes);
    assert!(!temporary.exists());
    assert_eq!(
        load_or_create_identity(&fixture.dir(), WAIT)
            .unwrap()
            .endpoint_id(),
        identity.endpoint_id()
    );
}

#[test]
fn forged_recovery_name_does_not_remove_unrelated_file_or_links() {
    let fixture = Fixture::new();
    let key = SecretKey::generate();
    fixture.write("machine.key", &key.to_bytes());
    fs::hard_link(fixture.0.join("machine.key"), fixture.0.join("unrelated")).unwrap();
    let temporary = format!(".identity-{}.tmp", key.public());
    fixture.write(&temporary, &key.to_bytes());
    assert!(matches!(
        load_or_create_identity(&fixture.dir(), WAIT),
        Err(IdentityError::UnsafeStorage)
    ));
    assert_eq!(
        fs::read(fixture.0.join(&temporary)).unwrap(),
        key.to_bytes()
    );
    assert_eq!(
        fs::read(fixture.0.join("unrelated")).unwrap(),
        key.to_bytes()
    );
}
