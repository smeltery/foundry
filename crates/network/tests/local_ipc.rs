#![cfg(unix)]
use cap_std::{ambient_authority, fs::Dir};
use foundry_network::{
    daemon_lease::DaemonLease,
    ipc_codec::{read_record, write_record},
    local_ipc::LocalListener,
};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    time::Duration,
};
use tokio::{io::BufReader, net::UnixStream};
const DEADLINE: Duration = Duration::from_secs(2);
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        // Keep ordinary roundtrip cases below the native sockaddr_un limit.
        let path =
            PathBuf::from("/tmp").join(format!("ipc-{}", iroh::SecretKey::generate().public()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn lease(&self) -> DaemonLease {
        DaemonLease::acquire(&Dir::open_ambient_dir(&self.0, ambient_authority()).unwrap()).unwrap()
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[tokio::test]
async fn long_state_path_uses_explicit_private_socket_directory() {
    let root = Root::new();
    let short = Root::new();
    let long = root.0.join("state".repeat(30));
    fs::create_dir(&long).unwrap();
    fs::set_permissions(&long, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = Dir::open_ambient_dir(&long, ambient_authority()).unwrap();
    let lease = DaemonLease::acquire(&directory).unwrap();
    let listener = LocalListener::bind_at(&long, &short.0, lease, DEADLINE)
        .await
        .unwrap();
    let _client = UnixStream::connect(listener.path()).await.unwrap();
    let _server = listener.accept(DEADLINE).await.unwrap();
    assert!(!long.join("daemon.sock").exists());
    drop(listener);
    assert!(!short.0.join("daemon.sock").exists());
    assert!(DaemonLease::acquire(&directory).is_ok());
}

#[tokio::test]
async fn real_private_socket_roundtrip_and_shutdown_cleanup() {
    let root = Root::new();
    let listener = LocalListener::bind(&root.0, root.lease(), DEADLINE)
        .await
        .unwrap();
    let path = listener.path().to_owned();
    let mut client = UnixStream::connect(&path).await.unwrap();
    let mut server = listener.accept(DEADLINE).await.unwrap();
    write_record(&mut client, &42, DEADLINE).await.unwrap();
    assert_eq!(
        read_record::<u64>(&mut BufReader::new(&mut server), DEADLINE)
            .await
            .unwrap(),
        Some(42)
    );
    write_record(&mut server, &43, DEADLINE).await.unwrap();
    assert_eq!(
        read_record::<u64>(&mut BufReader::new(&mut client), DEADLINE)
            .await
            .unwrap(),
        Some(43)
    );
    drop(listener);
    assert!(!path.exists());
    let _lease = root.lease();
}

#[tokio::test]
async fn stale_socket_recovers_but_live_socket_and_other_files_survive() {
    let root = Root::new();
    let path = root.0.join("daemon.sock");
    let old = std::os::unix::net::UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        LocalListener::bind(&root.0, root.lease(), DEADLINE)
            .await
            .is_err()
    );
    assert!(path.exists());
    drop(old);
    let listener = LocalListener::bind(&root.0, root.lease(), DEADLINE)
        .await
        .unwrap();
    drop(listener);
    fs::write(&path, b"sentinel").unwrap();
    assert!(
        LocalListener::bind(&root.0, root.lease(), DEADLINE)
            .await
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), b"sentinel");
    fs::remove_file(&path).unwrap();
    symlink(root.0.join("daemon.lock"), &path).unwrap();
    assert!(
        LocalListener::bind(&root.0, root.lease(), DEADLINE)
            .await
            .is_err()
    );
    assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
}

#[tokio::test]
async fn unrelated_lease_is_rejected_and_replaced_socket_not_removed_on_drop() {
    let root = Root::new();
    let other = Root::new();
    assert!(
        LocalListener::bind(&root.0, other.lease(), DEADLINE)
            .await
            .is_err()
    );
    assert!(!root.0.join("daemon.sock").exists());
    let listener = LocalListener::bind(&root.0, root.lease(), DEADLINE)
        .await
        .unwrap();
    assert!(listener.accept(Duration::from_millis(10)).await.is_err());
    let path = listener.path().to_owned();
    fs::remove_file(&path).unwrap();
    fs::write(&path, b"replacement").unwrap();
    drop(listener);
    assert_eq!(fs::read(&path).unwrap(), b"replacement");
}

#[tokio::test]
async fn replacement_socket_remains_connectable_after_listener_drop() {
    use std::os::unix::fs::MetadataExt;
    let root = Root::new();
    let listener = LocalListener::bind(&root.0, root.lease(), DEADLINE)
        .await
        .unwrap();
    let path = listener.path().to_owned();
    let original_inode = fs::metadata(&path).unwrap().ino();
    fs::remove_file(&path).unwrap();
    let replacement = tokio::net::UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let replacement_inode = fs::metadata(&path).unwrap().ino();
    assert_ne!(original_inode, replacement_inode);
    drop(listener);
    assert_eq!(fs::metadata(&path).unwrap().ino(), replacement_inode);
    let mut client = UnixStream::connect(&path).await.unwrap();
    let (mut server, _) = replacement.accept().await.unwrap();
    write_record(&mut client, &91, DEADLINE).await.unwrap();
    assert_eq!(
        read_record::<u64>(&mut BufReader::new(&mut server), DEADLINE)
            .await
            .unwrap(),
        Some(91)
    );
    // Refuse to recover the live replacement, then recover it after it closes.
    assert!(
        LocalListener::bind(&root.0, root.lease(), DEADLINE)
            .await
            .is_err()
    );
    drop(server);
    drop(client);
    drop(replacement);
    let restarted = LocalListener::bind(&root.0, root.lease(), DEADLINE)
        .await
        .unwrap();
    let mut client = UnixStream::connect(restarted.path()).await.unwrap();
    let mut server = restarted.accept(DEADLINE).await.unwrap();
    write_record(&mut client, &92, DEADLINE).await.unwrap();
    assert_eq!(
        read_record::<u64>(&mut BufReader::new(&mut server), DEADLINE)
            .await
            .unwrap(),
        Some(92)
    );
    drop(restarted);
    assert!(!path.exists());
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1); // Retained lease only.
}

#[tokio::test]
async fn separate_state_roots_cannot_share_an_owned_socket_namespace() {
    let first = Root::new();
    let second = Root::new();
    let sockets = Root::new();
    let listener = LocalListener::bind_at(&first.0, &sockets.0, first.lease(), DEADLINE)
        .await
        .unwrap();
    // Even with its canonical entry accidentally absent, the namespace stays leased.
    let saved = sockets.0.join("saved.sock");
    fs::rename(listener.path(), &saved).unwrap();
    let error = LocalListener::bind_at(&second.0, &sockets.0, second.lease(), DEADLINE)
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("socket directory lease"));
    assert!(!sockets.0.join("daemon.sock").exists());
    fs::rename(&saved, listener.path()).unwrap();
    let _client = UnixStream::connect(listener.path()).await.unwrap();
    let _server = listener.accept(DEADLINE).await.unwrap();
    drop(listener);
    let restarted = LocalListener::bind_at(&second.0, &sockets.0, second.lease(), DEADLINE)
        .await
        .unwrap();
    let _client = UnixStream::connect(restarted.path()).await.unwrap();
    let _server = restarted.accept(DEADLINE).await.unwrap();
    drop(restarted);
    assert_eq!(fs::read_dir(&sockets.0).unwrap().count(), 1);
}
