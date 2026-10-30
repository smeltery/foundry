//! Foreground local IPC and explicitly configured peer runtime owner.
use crate::{
    daemon_config::DaemonConfig,
    daemon_engine::{
        ConnectionId, Engine, EngineError, ReplyResult, RequestEnvelope, TrustedIdentity,
    },
    daemon_lease::DaemonLease,
    ipc_codec::{read_record, write_record},
    local_ipc::LocalListener,
};
use cap_std::{ambient_authority, fs::Dir};
use std::{
    fmt,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::BufReader,
    net::UnixStream,
    sync::{Semaphore, watch},
    task::JoinSet,
};

#[path = "daemon_host/cancellation.rs"]
mod cancellation;
#[path = "daemon_host/connection.rs"]
mod connection;
pub(crate) mod network;
#[path = "daemon_host/response.rs"]
mod response;
#[path = "daemon_host/spool.rs"]
mod spool;
#[path = "daemon_host/startup.rs"]
mod startup;
#[path = "daemon_host/waiting.rs"]
mod waiting;
pub use cancellation::DaemonTask;
use cancellation::{RootFence, WaitRegistry};
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONNECTIONS: usize = 64;

#[derive(Debug, Clone, Copy)]
pub enum HostError {
    Configuration,
    Startup,
    Storage,
    Task,
    Signal,
}
impl fmt::Display for HostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Configuration => "daemon configuration rejected",
            Self::Startup => "daemon startup failed",
            Self::Storage => "daemon storage requires recovery",
            Self::Task => "daemon worker failed",
            Self::Signal => "daemon signal handling failed",
        })
    }
}
impl std::error::Error for HostError {}

pub struct Daemon {
    listener: LocalListener,
    engine: Arc<OwnedEngine>,
    cancellation: Arc<WaitRegistry>,
    network: Option<crate::daemon_network::NetworkHandle>,
    network_owner: Option<crate::daemon_network::NetworkOwner>,
}
struct OwnedEngine {
    inner: Mutex<Engine>,
    replies: Arc<Semaphore>,
    root: Dir,
    // Blocking work cannot be cancelled once started. Its actual Arc lifetime,
    // including initialization, must retain ownership after a host future drops.
    _lease: DaemonLease,
}
impl Daemon {
    pub async fn open(config: DaemonConfig) -> Result<Self, HostError> {
        startup::open(config, None).await
    }
    pub async fn open_from(config: DaemonConfig, reference_base: &Path) -> Result<Self, HostError> {
        startup::open(config, Some(reference_base)).await
    }

    /// Prefer this owned handle when an external task must abort the daemon.
    pub fn spawn(self) -> DaemonTask {
        let registry = self.cancellation.clone();
        DaemonTask {
            task: tokio::spawn(self.serve()),
            registry,
        }
    }
    pub fn path(&self) -> &Path {
        self.listener.path()
    }

    /// Bound connection and worker counts independently of input. SQLite work is
    /// serialized off the async executor. Every accepted connection gets a fresh
    /// host-owned ID, and cleanup completes before releasing directory ownership.
    pub async fn serve(self) -> Result<(), HostError> {
        let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let (shutdown, mut stopping) = watch::channel(false);
        // Field order is deliberate: close wait gates before JoinSet requests
        // deferred child cancellation on ancestor drop.
        struct Workers {
            fence: RootFence,
            tasks: JoinSet<Result<(), HostError>>,
        }
        let mut workers = Workers {
            fence: RootFence(self.cancellation.clone()),
            tasks: JoinSet::new(),
        };
        let mut next_id = 1u64;
        let signal = tokio::signal::ctrl_c();
        tokio::pin!(signal);
        let mut termination =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|_| HostError::Signal)?;
        let mut result = Ok(());
        let mut network_stop = self.network.as_ref().map(|network| network.stopping());
        loop {
            tokio::select! {
                _ = async {
                    if let Some(stop)=&mut network_stop {
                        if !*stop.borrow() {let _=stop.changed().await;}
                    } else {std::future::pending::<()>().await;}
                }=>{result=Err(HostError::Task);break;}
                changed = stopping.changed() => {
                    if changed.is_err() || *stopping.borrow() { break; }
                }
                signal_result = &mut signal => {
                    if signal_result.is_err() { result = Err(HostError::Signal); }
                    break;
                }
                received = termination.recv() => {
                    if received.is_none() { result = Err(HostError::Signal); }
                    break;
                }
                finished = workers.tasks.join_next(), if !workers.tasks.is_empty() => {
                    if !matches!(finished, Some(Ok(Ok(())))) {
                        result = Err(HostError::Task);
                        break;
                    }
                }
                accepted = self.listener.accept(Duration::from_secs(1)) => {
                    match accepted {
                        Ok(stream) => {
                            let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                            let connection = ConnectionId(next_id);
                            let Some(next) = next_id.checked_add(1) else {
                                result = Err(HostError::Task); break;
                            };
                            next_id = next;
                            let fence = self.cancellation.connection(connection);
                            let engine = self.engine.clone();
                            let shutdown = shutdown.clone();
                            let network = self.network.clone();
                            workers.tasks.spawn(async move {
                                let _permit = permit;
                                connection::run(stream, engine, connection, shutdown, fence, network).await
                            });
                        }
                        Err(crate::NetworkError::TimedOut) => {}
                        Err(_) => { result = Err(HostError::Task); break; }
                    }
                }
            }
        }
        workers.fence.0.cancel_all();
        let _ = shutdown.send(true);
        while let Some(finished) = workers.tasks.join_next().await {
            if !matches!(finished, Ok(Ok(()))) {
                result = Err(HostError::Task);
            }
        }
        drop(self.network);
        if let Some(owner) = self.network_owner
            && owner.shutdown().await.is_err()
        {
            result = Err(HostError::Task);
        }
        result
    }
}
