use super::*;
use std::{
    cell::RefCell,
    collections::VecDeque,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    rc::Rc,
};

struct Hook {
    path: PathBuf,
    opened: bool,
    action: Box<dyn FnOnce()>,
}
thread_local! {
    static HOOKS: RefCell<VecDeque<Hook>> = const { RefCell::new(VecDeque::new()) };
}
pub(super) fn checkpoint(path: &Path, opened: bool) {
    let hook = HOOKS.with(|hooks| {
        let mut hooks = hooks.borrow_mut();
        if hooks
            .front()
            .is_some_and(|h| h.path == path && h.opened == opened)
        {
            hooks.pop_front()
        } else {
            None
        }
    });
    if let Some(hook) = hook {
        (hook.action)();
    }
}
fn pause(path: PathBuf, opened: bool, action: impl FnOnce() + 'static) {
    HOOKS.with(|hooks| {
        hooks.borrow_mut().push_back(Hook {
            path,
            opened,
            action: Box::new(action),
        })
    });
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "sidecar-lifecycle-{:032x}",
            u128::from_ne_bytes(nonce)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn journal(&self) -> PathBuf {
        self.0.join("mail.sqlite3-journal")
    }
    fn setup(&self) -> (Protection, Rc<Connection>) {
        let protection =
            Protection::open_named(&self.0, "mail.sqlite3", OpenMode::CreateOrOpen).unwrap();
        let writer = Rc::new(Connection::open(protection.path()).unwrap());
        writer.execute_batch("PRAGMA journal_mode=DELETE; CREATE TABLE sample(value INTEGER); INSERT INTO sample VALUES(1); BEGIN IMMEDIATE; UPDATE sample SET value=value+1").unwrap();
        assert!(self.journal().exists());
        (protection, writer)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        HOOKS.with(|hooks| hooks.borrow_mut().clear());
        SNAPSHOTS.with(|snapshots| snapshots.borrow_mut().clear());
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn commit_during_check(opened: bool) {
    let root = Root::new();
    let (store, writer) = root.setup();
    let committing = writer.clone();
    pause(root.journal(), opened, move || {
        committing.execute_batch("COMMIT").unwrap()
    });
    let result = store.check();
    assert!(HOOKS.with(|hooks| hooks.borrow().is_empty()));
    assert!(!root.journal().exists());
    result.unwrap();
    drop((store, writer));
    let reopened = Connection::open(root.0.join("mail.sqlite3")).unwrap();
    let value: i64 = reopened
        .query_row("SELECT value FROM sample", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, 2);
}
#[test]
fn sqlite_commit_between_sidecar_stat_and_open_keeps_database_readable() {
    commit_during_check(false);
}
#[test]
fn sqlite_commit_after_sidecar_open_keeps_database_readable() {
    commit_during_check(true);
}
#[test]
fn recreated_sqlite_journal_is_checked_as_a_new_generation() {
    for opened in [false, true] {
        let root = Root::new();
        let (store, writer) = root.setup();
        let committing = writer.clone();
        pause(root.journal(), opened, move || {
            committing
                .execute_batch("COMMIT; BEGIN IMMEDIATE; UPDATE sample SET value=value+1")
                .unwrap();
        });
        store.check().unwrap();
        writer.execute_batch("COMMIT").unwrap();
    }
}
#[test]
fn replacement_symlinks_hardlinks_and_permissions_still_fail_closed() {
    for (opened, kind) in [false, true]
        .into_iter()
        .flat_map(|opened| ["symlink", "hardlink", "permissions"].map(|kind| (opened, kind)))
    {
        let root = Root::new();
        let (store, writer) = root.setup();
        let sentinel = root.0.join("sentinel");
        fs::write(&sentinel, b"untouched").unwrap();
        fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o600)).unwrap();
        let journal = root.journal();
        let replacement = journal.clone();
        let target = sentinel.clone();
        pause(journal, opened, move || {
            writer.execute_batch("COMMIT").unwrap();
            match kind {
                "symlink" => symlink(&target, &replacement).unwrap(),
                "hardlink" => fs::hard_link(&target, &replacement).unwrap(),
                _ => {
                    fs::write(&replacement, b"").unwrap();
                    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o644)).unwrap();
                }
            }
        });
        assert!(matches!(
            store.check(),
            Err(PersistenceError::UnsafeStorage)
        ));
        assert_eq!(fs::read(&sentinel).unwrap(), b"untouched");
        fs::remove_file(root.journal()).unwrap();
    }
}
#[test]
fn unlinked_journal_with_changed_permissions_is_not_accepted_as_retired() {
    let root = Root::new();
    let (store, writer) = root.setup();
    let held = fs::File::open(root.journal()).unwrap();
    pause(root.journal(), true, move || {
        writer.execute_batch("COMMIT").unwrap();
        held.set_permissions(fs::Permissions::from_mode(0o644))
            .unwrap();
    });
    assert!(matches!(
        store.check(),
        Err(PersistenceError::UnsafeStorage)
    ));
}

#[test]
fn continuous_private_generation_changes_return_explicit_busy() {
    let root = Root::new();
    let (store, writer) = root.setup();
    let retired = Rc::new(RefCell::new(Vec::new()));
    for _ in 0..4 {
        let committing = writer.clone();
        let retained = retired.clone();
        let journal = root.journal();
        pause(journal.clone(), false, move || {
            // Retain the old inode so the new journal cannot reuse its number.
            retained
                .borrow_mut()
                .push(fs::File::open(&journal).unwrap());
            committing
                .execute_batch("COMMIT; BEGIN IMMEDIATE; UPDATE sample SET value=value+1")
                .unwrap();
        });
    }
    assert!(matches!(store.check(), Err(PersistenceError::Busy)));
    assert!(HOOKS.with(|hooks| hooks.borrow().is_empty()));
    writer.execute_batch("COMMIT").unwrap();
    store.check().unwrap();
}

struct Snapshot {
    path: PathBuf,
    current: bool,
    read: Box<dyn FnOnce() -> fs::Metadata>,
}
thread_local! {
    static SNAPSHOTS: RefCell<VecDeque<Snapshot>> = const { RefCell::new(VecDeque::new()) };
}
pub(super) fn snapshot(path: &Path, current: bool, original: fs::Metadata) -> fs::Metadata {
    let selected = SNAPSHOTS.with(|snapshots| {
        let mut snapshots = snapshots.borrow_mut();
        if snapshots
            .front()
            .is_some_and(|s| s.path == path && s.current == current)
        {
            snapshots.pop_front()
        } else {
            None
        }
    });
    selected.map_or(original, |snapshot| (snapshot.read)())
}
fn retired_snapshot(current: bool, unsafe_mode: bool) {
    let root = Root::new();
    let (store, writer) = root.setup();
    let held = fs::File::open(root.journal()).unwrap();
    SNAPSHOTS.with(|snapshots| {
        snapshots.borrow_mut().push_back(Snapshot {
            path: root.journal(),
            current,
            read: Box::new(move || {
                writer.execute_batch("COMMIT").unwrap();
                if unsafe_mode {
                    held.set_permissions(fs::Permissions::from_mode(0o644))
                        .unwrap();
                }
                // Actual kernel metadata from SQLite's just-retired inode, not
                // constructed fields. Models unlink after pathname resolution.
                held.metadata().unwrap()
            }),
        })
    });
    let result = store.check();
    assert!(SNAPSHOTS.with(|snapshots| snapshots.borrow().is_empty()));
    if unsafe_mode {
        assert!(matches!(result, Err(PersistenceError::UnsafeStorage)));
    } else {
        result.unwrap();
    }
}
#[test]
fn retired_initial_stat_rechecks_current_path() {
    retired_snapshot(false, false);
}
#[test]
fn retired_final_stat_rechecks_current_path() {
    retired_snapshot(true, false);
}
#[test]
fn retired_path_snapshots_with_unsafe_mode_are_rejected() {
    for current in [false, true] {
        retired_snapshot(current, true);
    }
}
#[test]
fn two_real_commits_retire_the_newly_opened_generation() {
    let root = Root::new();
    let (store, writer) = root.setup();
    let retained = fs::File::open(root.journal()).unwrap();
    let first = writer.clone();
    pause(root.journal(), false, move || {
        first
            .execute_batch("COMMIT; BEGIN IMMEDIATE; UPDATE sample SET value=value+1")
            .unwrap();
    });
    let second = writer.clone();
    pause(root.journal(), true, move || {
        second.execute_batch("COMMIT").unwrap()
    });
    store.check().unwrap();
    assert!(HOOKS.with(|hooks| hooks.borrow().is_empty()));
    drop(retained);
    let value: i64 = writer
        .query_row("SELECT value FROM sample", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, 3);
}
#[test]
fn observed_main_database_disappearance_cannot_create_a_replacement() {
    let root = Root::new();
    let path = root.0.join("main.sqlite3");
    fs::write(&path, b"original").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let old = root.0.join("preserved.sqlite3");
    let removed = path.clone();
    let preserved = old.clone();
    pause(path.clone(), false, move || {
        fs::rename(removed, preserved).unwrap()
    });
    assert!(matches!(
        Protection::open_named(&root.0, "main.sqlite3", OpenMode::CreateOrOpen),
        Err(PersistenceError::UnsafeStorage)
    ));
    assert!(!path.exists());
    assert_eq!(fs::read(old).unwrap(), b"original");
}
