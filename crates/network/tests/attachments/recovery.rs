use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};
#[test]
fn partial_materialization_error_preserves_reservation_and_resumes_exact_prefix() {
    let root = Root::new();
    let outside = Root::new();
    outside.write("sentinel", "keep");
    let owner = key("partial");
    let dir = directory(&owner);
    root.mkdir(&dir);
    fs::set_permissions(root.0.join(&dir), fs::Permissions::from_mode(0o700)).unwrap();
    symlink(
        outside.0.join("sentinel"),
        root.0.join(&dir).join(".part-1"),
    )
    .unwrap();
    let files = [file("first.txt", "first"), file("second.txt", "complete")];
    let mut store = AttachmentStore::open(&root.0).unwrap();
    assert!(store.persist(&owner, &files, 0).is_err());
    assert_eq!(
        store.usage().unwrap(),
        Usage {
            batches: 1,
            bytes: 13
        }
    );
    assert_eq!(
        fs::read_to_string(root.0.join(&dir).join("first.txt")).unwrap(),
        "first"
    );
    assert_eq!(
        fs::read_to_string(outside.0.join("sentinel")).unwrap(),
        "keep"
    );
    drop(store);
    fs::remove_file(root.0.join(&dir).join(".part-1")).unwrap();
    private_file(&root, &format!("{dir}/.part-1"), "comp");
    let mut store = AttachmentStore::open(&root.0).unwrap();
    let saved = store.persist(&owner, &files, 1).unwrap();
    assert_eq!(
        fs::read_to_string(saved[1].path.as_ref().unwrap()).unwrap(),
        "complete"
    );
    assert!(!root.0.join(&dir).join(".part-1").exists());
    assert_eq!(
        store.usage().unwrap(),
        Usage {
            batches: 1,
            bytes: 13
        }
    );
}
#[test]
fn conflicting_partial_bytes_final_bytes_symlinks_and_hardlinks_are_preserved() {
    let root = Root::new();
    let outside = Root::new();
    let mut store = AttachmentStore::open(&root.0).unwrap();
    let owner = key("collision");
    let saved = store
        .persist(&owner, &[file("f.txt", "expected")], 0)
        .unwrap();
    let path = PathBuf::from(saved[0].path.as_ref().unwrap());
    fs::write(&path, "unexpected").unwrap();
    assert_eq!(
        store.recover(&owner).unwrap_err(),
        AttachmentError::Conflict
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "unexpected");
    fs::remove_file(&path).unwrap();
    outside.write("target", "external");
    symlink(outside.0.join("target"), &path).unwrap();
    assert!(store.recover(&owner).is_err());
    assert_eq!(
        fs::read_to_string(outside.0.join("target")).unwrap(),
        "external"
    );
    fs::remove_file(&path).unwrap();
    fs::hard_link(outside.0.join("target"), &path).unwrap();
    assert!(store.recover(&owner).is_err());
    fs::remove_file(&path).unwrap();
    private_file(
        &root,
        &format!("{}/.part-0", directory(&owner)),
        "wrong-prefix",
    );
    assert_eq!(
        store.recover(&owner).unwrap_err(),
        AttachmentError::Conflict
    );
}
#[test]
fn publication_interruption_recovers_two_links_without_overwriting_files() {
    let root = Root::new();
    let mut store = AttachmentStore::open(&root.0).unwrap();
    let owner = key("publish");
    let saved = store
        .persist(&owner, &[file("n.txt", "original")], 0)
        .unwrap();
    let path = PathBuf::from(saved[0].path.as_ref().unwrap());
    let partial = path.parent().unwrap().join(".part-0");
    fs::hard_link(&path, &partial).unwrap();
    assert_eq!(store.recover(&owner).unwrap(), saved);
    assert!(!partial.exists());
}
#[test]
fn cleanup_requires_explicit_release_retention_and_never_deletes_unknown_entries() {
    let root = Root::new();
    let mut store = AttachmentStore::open(&root.0).unwrap();
    let owner = key("retained");
    let files = [file("n.txt", "kept")];
    let saved = store.persist(&owner, &files, 10).unwrap();
    assert_eq!(
        store.cleanup(&owner, RETAIN_RELEASED_MS * 8).unwrap_err(),
        AttachmentError::InUse
    );
    assert!(store.recover(&owner).is_ok());
    store.release(&owner, 20).unwrap();
    assert_eq!(
        store.cleanup(&owner, 20).unwrap_err(),
        AttachmentError::Retention
    );
    let extra = root.0.join(directory(&owner)).join("unknown.txt");
    fs::write(&extra, "untouched").unwrap();
    assert_eq!(
        store.cleanup(&owner, 20 + RETAIN_RELEASED_MS).unwrap_err(),
        AttachmentError::Conflict
    );
    assert_eq!(fs::read_to_string(&extra).unwrap(), "untouched");
    assert_eq!(
        store.recover(&owner).unwrap_err(),
        AttachmentError::Deleting
    );
    assert_eq!(
        store
            .persist(&owner, &files, RETAIN_RELEASED_MS + 20)
            .unwrap_err(),
        AttachmentError::Deleting
    );
    drop(store);
    fs::remove_file(extra).unwrap();
    let mut store = AttachmentStore::open(&root.0).unwrap();
    store.cleanup(&owner, 20 + RETAIN_RELEASED_MS).unwrap();
    assert!(!PathBuf::from(saved[0].path.as_ref().unwrap()).exists());
    assert_eq!(
        store.usage().unwrap(),
        Usage {
            batches: 0,
            bytes: 0
        }
    );
    assert_eq!(store.recover(&owner).unwrap_err(), AttachmentError::Missing);
}
#[test]
fn receiver_directory_symlink_and_public_root_are_refused() {
    let root = Root::new();
    let outside = Root::new();
    let owner = key("escape");
    symlink(&outside.0, root.0.join(directory(&owner))).unwrap();
    let mut store = AttachmentStore::open(&root.0).unwrap();
    assert!(
        store
            .persist(&owner, &[file("safe.txt", "no escape")], 0)
            .is_err()
    );
    assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 0);
    drop(store);
    fs::set_permissions(&root.0, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(AttachmentStore::open(&root.0).is_err());
}

#[test]
fn cleanup_recovers_publication_interruption_but_preserves_changed_file_bytes() {
    let root = Root::new();
    let mut store = AttachmentStore::open(&root.0).unwrap();
    let owner = key("delete-collision");
    let saved = store.persist(&owner, &[file("a.txt", "owned")], 0).unwrap();
    let path = PathBuf::from(saved[0].path.as_ref().unwrap());
    store.release(&owner, 0).unwrap();
    fs::write(&path, "replacement").unwrap();
    assert_eq!(
        store.cleanup(&owner, RETAIN_RELEASED_MS).unwrap_err(),
        AttachmentError::Conflict
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "replacement");
    fs::write(&path, "owned").unwrap();
    fs::hard_link(&path, path.parent().unwrap().join(".part-0")).unwrap();
    store.cleanup(&owner, RETAIN_RELEASED_MS).unwrap();
    assert!(!path.exists());
}
#[cfg(target_os = "macos")]
#[test]
fn receiver_files_carry_download_quarantine_on_macos() {
    use std::os::fd::AsRawFd;
    let root = Root::new();
    let mut store = AttachmentStore::open(&root.0).unwrap();
    let saved = store
        .persist(&key("download"), &[file("a.txt", "text")], 0)
        .unwrap();
    let f = fs::File::open(saved[0].path.as_ref().unwrap()).unwrap();
    let mut data = [0u8; 128];
    // The writable buffer and attribute name stay live for this descriptor call.
    let n = unsafe {
        libc::fgetxattr(
            f.as_raw_fd(),
            c"com.apple.quarantine".as_ptr(),
            data.as_mut_ptr().cast(),
            data.len(),
            0,
            0,
        )
    };
    assert!(n > 0);
    assert!(
        std::str::from_utf8(&data[..n as usize])
            .unwrap()
            .starts_with("0081;")
    );
}

#[test]
fn concurrent_store_handles_serialize_exact_retry_without_double_reservation() {
    let root = Root::new();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let path = root.0.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            let mut store = AttachmentStore::open(&path).unwrap();
            barrier.wait();
            store
                .persist(&key("concurrent"), &[file("data.txt", "one payload")], 0)
                .unwrap()
        }));
    }
    let first = workers.remove(0).join().unwrap();
    assert_eq!(first, workers.remove(0).join().unwrap());
    let store = AttachmentStore::open(&root.0).unwrap();
    assert_eq!(
        store.usage().unwrap(),
        Usage {
            batches: 1,
            bytes: 11
        }
    );
}
#[test]
fn sender_fifo_is_rejected_without_a_blocking_read() {
    use std::os::unix::ffi::OsStrExt;
    let root = Root::new();
    let path = root.0.join("pipe.txt");
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // Creates only a named pipe inside this test's fresh private directory.
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    assert!(prepare(&root.policy(), &["pipe.txt".into()]).is_err());
}

#[test]
fn receiver_files_and_database_keep_single_link_protection() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = Root::new();
    let owner = key("private-links");
    let mut store = AttachmentStore::open(&root.0).unwrap();
    let saved = store
        .persist(&owner, &[file("owned.txt", "fixture bytes")], 0)
        .unwrap();
    let path = PathBuf::from(saved[0].path.as_ref().unwrap());
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    fs::hard_link(&path, root.0.join("extra-file-link.txt")).unwrap();
    assert!(fs::metadata(&path).unwrap().nlink() > 1);
    assert_eq!(
        store.recover(&owner).unwrap_err(),
        AttachmentError::UnsafePath
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), "fixture bytes");
    let database = root.0.join("attachments.sqlite3");
    assert_eq!(
        fs::metadata(&database).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    fs::hard_link(&database, root.0.join("extra-database-link")).unwrap();
    assert_eq!(store.usage().unwrap_err(), AttachmentError::UnsafePath);
    assert!(AttachmentStore::open(&root.0).is_err());
}
