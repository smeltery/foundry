use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let mut random = [0; 12];
        getrandom::fill(&mut random).unwrap();
        let path = PathBuf::from("/tmp").join(data_encoding::BASE64URL_NOPAD.encode(&random));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn live(path: &Path) -> UnixListener {
    let listener = UnixListener::bind(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    listener
}
fn inode(path: &Path) -> Identity {
    identity(&fs::symlink_metadata(path).unwrap())
}
async fn roundtrip(listener: &UnixListener, path: &Path) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let mut client = UnixStream::connect(path).await.unwrap();
        let (mut server, _) = listener.accept().await.unwrap();
        client.write_all(b"ping").await.unwrap();
        let mut bytes = [0; 4];
        server.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"ping");
        server.write_all(b"pong").await.unwrap();
        client.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"pong");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn private_binding_records_inode_before_publication_and_never_adopts_replacement() {
    let root = Root::new();
    let mut owner = SocketPath::new(&root.0).unwrap();
    assert_eq!(
        fs::metadata(&owner.directory).unwrap().mode() & 0o7777,
        0o700
    );
    owner.bind().unwrap();
    let expected = owner.identity().unwrap();
    assert_eq!(inode(&owner.staged_path()), expected);
    let replacement = live(&owner.canonical);
    fs::set_permissions(&owner.canonical, fs::Permissions::from_mode(0o640)).unwrap();
    let replacement_inode = inode(&owner.canonical);
    assert_ne!(expected, replacement_inode);
    assert!(matches!(owner.publish(), Err(NetworkError::Io(error))
        if error.kind() == io::ErrorKind::AlreadyExists));
    let path = owner.canonical.clone();
    drop(owner);
    assert_eq!(inode(&path), replacement_inode);
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o640);
    roundtrip(&replacement, &path).await;
}

#[tokio::test]
async fn published_inode_survives_rename_and_roundtrips_then_cleans_up() {
    let root = Root::new();
    let mut owner = SocketPath::new(&root.0).unwrap();
    owner.bind().unwrap();
    let expected = owner.identity().unwrap();
    owner.publish().unwrap();
    assert_eq!(inode(&owner.canonical), expected);
    assert_eq!(fs::metadata(&owner.canonical).unwrap().nlink(), 1);
    roundtrip(owner.listener().unwrap(), &owner.canonical).await;
    let generation = owner.directory.clone();
    let path = owner.canonical.clone();
    drop(owner);
    assert!(!path.exists());
    assert!(!generation.exists());
}

#[tokio::test]
async fn replacement_after_publish_is_restored_and_remains_live_on_drop() {
    let root = Root::new();
    let mut owner = SocketPath::new(&root.0).unwrap();
    owner.bind().unwrap();
    owner.publish().unwrap();
    let path = owner.canonical.clone();
    fs::remove_file(&path).unwrap();
    let replacement = live(&path);
    let replacement_inode = inode(&path);
    assert_ne!(owner.identity().unwrap(), replacement_inode);
    drop(owner);
    assert_eq!(inode(&path), replacement_inode);
    roundtrip(&replacement, &path).await;
}

#[tokio::test]
async fn cleanup_compares_moved_socket_instead_of_deleting_replacement() {
    let root = Root::new();
    let owner = SocketPath::new(&root.0).unwrap();
    let original = live(&owner.canonical);
    let expected = inode(&owner.canonical);
    drop(original);
    // Deterministically replace the name after the recovery caller's snapshot.
    // Keep the original inode linked elsewhere to rule out accidental inode reuse.
    let old_path = root.0.join("old");
    fs::rename(&owner.canonical, &old_path).unwrap();
    let replacement = live(&owner.canonical);
    let replacement_inode = inode(&owner.canonical);
    assert!(!owner.remove_expected(expected).unwrap());
    assert_eq!(inode(&owner.canonical), replacement_inode);
    roundtrip(&replacement, &owner.canonical).await;
}

#[tokio::test]
async fn replacement_after_quarantine_move_survives_owned_socket_removal() {
    let root = Root::new();
    let owner = SocketPath::new(&root.0).unwrap();
    let original = live(&owner.canonical);
    let expected = inode(&owner.canonical);
    let mut replacement = None;
    assert!(
        owner
            .remove_with(expected, || {
                replacement = Some(live(&owner.canonical));
            })
            .unwrap()
    );
    let replacement = replacement.unwrap();
    assert_ne!(inode(&owner.canonical), expected);
    roundtrip(&replacement, &owner.canonical).await;
    drop(original);
}

#[tokio::test]
async fn occupied_restore_preserves_both_live_sockets_and_reports_quarantine_path() {
    let root = Root::new();
    let owner = SocketPath::new(&root.0).unwrap();
    let original = live(&owner.canonical);
    let expected = inode(&owner.canonical);
    fs::rename(&owner.canonical, root.0.join("original.sock")).unwrap();
    let unexpected = live(&owner.canonical);
    let unexpected_inode = inode(&owner.canonical);
    let mut newer = None;
    let error = owner
        .remove_with(expected, || {
            newer = Some(live(&owner.canonical));
        })
        .unwrap_err();
    let quarantine = owner.directory.join("q");
    assert!(
        error
            .to_string()
            .contains(&quarantine.display().to_string())
    );
    assert_eq!(inode(&quarantine), unexpected_inode);
    roundtrip(&unexpected, &quarantine).await;
    let path = owner.canonical.clone();
    let newer_inode = inode(&path);
    drop(owner);
    assert_eq!(inode(&quarantine), unexpected_inode);
    assert_eq!(inode(&path), newer_inode);
    roundtrip(&newer.unwrap(), &path).await;
    drop(original);
}

#[tokio::test]
async fn occupied_symlink_is_neither_followed_nor_chmodded() {
    let root = Root::new();
    let mut owner = SocketPath::new(&root.0).unwrap();
    owner.bind().unwrap();
    let target = root.0.join("sentinel");
    fs::write(&target, b"untouched").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    symlink(&target, &owner.canonical).unwrap();
    assert!(owner.publish().is_err());
    drop(owner);
    assert_eq!(fs::metadata(&target).unwrap().mode() & 0o7777, 0o644);
    assert_eq!(fs::read(&target).unwrap(), b"untouched");
    assert!(
        fs::symlink_metadata(root.0.join("daemon.sock"))
            .unwrap()
            .is_symlink()
    );
}

#[tokio::test]
async fn recovery_probes_replacement_socket_in_private_directory_and_preserves_it_if_live() {
    let root = Root::new();
    let owner = SocketPath::new(&root.0).unwrap();
    let original = live(&owner.canonical);
    drop(original);
    fs::rename(&owner.canonical, root.0.join("old.sock")).unwrap();
    let replacement = live(&owner.canonical);
    let expected = inode(&owner.canonical);
    assert!(
        owner
            .recover_existing(std::time::Duration::from_secs(2))
            .await
            .is_err()
    );
    assert_eq!(inode(&owner.canonical), expected);
    // Discard the successful liveness probe's queued connection first.
    let _probe = replacement.accept().await.unwrap();
    roundtrip(&replacement, &owner.canonical).await;
}

#[tokio::test]
async fn recovery_removes_only_refused_quarantined_socket_and_preserves_new_public_socket() {
    let root = Root::new();
    let owner = SocketPath::new(&root.0).unwrap();
    let original = live(&owner.canonical);
    drop(original);
    let mut replacement = None;
    owner
        .recover_with(std::time::Duration::from_secs(2), || {
            replacement = Some(live(&owner.canonical));
        })
        .await
        .unwrap();
    assert!(!owner.directory.join("q").exists());
    roundtrip(&replacement.unwrap(), &owner.canonical).await;
}
