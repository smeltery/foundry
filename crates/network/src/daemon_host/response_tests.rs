use super::*;
use crate::{daemon_lease::LeaseError, session_registry::FolderPolicy};
use cap_std::ambient_authority;
use std::{io::Write, os::unix::fs::PermissionsExt, path::PathBuf};

pub(in crate::daemon_host) struct State(pub PathBuf);
impl State {
    pub fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!(
            "response-test-{}",
            data_encoding::HEXLOWER.encode(&nonce)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(path.canonicalize().unwrap())
    }
    pub fn root(&self) -> Dir {
        Dir::open_ambient_dir(&self.0, ambient_authority()).unwrap()
    }
    pub fn engine(&self) -> Arc<OwnedEngine> {
        let root = self.root();
        let lease = DaemonLease::acquire(&root).unwrap();
        Arc::new(OwnedEngine {
            inner: Mutex::new(
                Engine::open(
                    &self.0,
                    FolderPolicy::default(),
                    TrustedIdentity {
                        person: "alice".into(),
                        machine: "desktop".into(),
                    },
                )
                .unwrap(),
            ),
            replies: Arc::new(Semaphore::new(1)),
            root,
            _lease: lease,
        })
    }
}
impl Drop for State {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn released(state: &State, slots: &Semaphore) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if slots.available_permits() == 1
                && let Ok(lease) = DaemonLease::acquire(&state.root())
            {
                drop(lease);
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!std::fs::read_dir(&state.0).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".reply-")
    }));
}
fn prepared(engine: Arc<OwnedEngine>, count: usize) -> PreparedReply {
    let slot = engine.replies.clone().try_acquire_owned().unwrap();
    let mut spool = spool::Spool::create(&engine.root, count).unwrap();
    spool.write_all(&vec![b'x'; count]).unwrap();
    spool.seal().unwrap();
    PreparedReply::new(
        spool,
        InboxSummary {
            messages: 1,
            bytes: count as u64,
        },
        slot,
        engine,
    )
    .unwrap()
}
#[tokio::test]
async fn queued_reply_and_cancelled_writer_retain_slot_and_actual_lease() {
    use tokio::io::AsyncReadExt;
    let state = State::new();
    let engine = state.engine();
    let slots = engine.replies.clone();
    let reply = prepared(engine.clone(), 256 * 1024);
    drop(engine);
    assert_eq!(slots.available_permits(), 0);
    assert!(matches!(
        DaemonLease::acquire(&state.root()),
        Err(LeaseError::AlreadyRunning)
    ));
    let (mut write, mut read) = tokio::io::duplex(1);
    let writing = tokio::spawn(async move { reply.write(&mut write).await });
    assert_eq!(read.read_u8().await.unwrap(), b'x');
    assert_eq!(slots.available_permits(), 0);
    writing.abort();
    assert!(writing.await.unwrap_err().is_cancelled());
    released(&state, &slots).await;
}
#[tokio::test]
async fn successful_stream_and_broken_pipe_release_all_owned_resources() {
    let state = State::new();
    for broken in [false, true] {
        let engine = state.engine();
        let slots = engine.replies.clone();
        let reply = prepared(engine.clone(), 128);
        drop(engine);
        if broken {
            let (mut writer, reader) = tokio::io::duplex(1);
            drop(reader);
            assert!(reply.write(&mut writer).await.is_err());
        } else {
            let mut writer = Vec::new();
            reply.write(&mut writer).await.unwrap();
            assert_eq!(writer, vec![b'x'; 128]);
        }
        released(&state, &slots).await;
    }
}

#[tokio::test]
async fn slot_survives_completed_file_reader_until_socket_flush_ends() {
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };
    struct FlushWait(Option<tokio::sync::oneshot::Sender<()>>);
    impl tokio::io::AsyncWrite for FlushWait {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            if let Some(signal) = self.0.take() {
                let _ = signal.send(());
            }
            Poll::Pending
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    let state = State::new();
    let engine = state.engine();
    let slots = engine.replies.clone();
    let reply = prepared(engine.clone(), 32);
    drop(engine);
    let (signal, flushing) = tokio::sync::oneshot::channel();
    let writing = tokio::spawn(async move { reply.write(&mut FlushWait(Some(signal))).await });
    tokio::time::timeout(Duration::from_secs(2), flushing)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(slots.available_permits(), 0);
    assert!(matches!(
        DaemonLease::acquire(&state.root()),
        Err(LeaseError::AlreadyRunning)
    ));
    writing.abort();
    assert!(matches!(writing.await, Err(error) if error.is_cancelled()));
    released(&state, &slots).await;
}

#[tokio::test]
async fn stalled_socket_uses_one_absolute_deadline_and_releases_ownership() {
    let state = State::new();
    let engine = state.engine();
    let slots = engine.replies.clone();
    let reply = prepared(engine.clone(), 256 * 1024);
    drop(engine);
    let (mut writer, _unread) = tokio::io::duplex(1);
    assert!(matches!(
        tokio::time::timeout(
            WRITE_TIMEOUT + Duration::from_secs(2),
            reply.write(&mut writer)
        )
        .await
        .unwrap(),
        Err(crate::NetworkError::TimedOut)
    ));
    released(&state, &slots).await;
}
