//! One explicit foreground network owner; all durable team changes use its gate.
mod bootstrap;
mod bootstrap_checks;
mod codes;
mod connections;
mod control;
mod dispatch;
mod driver;
mod events;
mod outbox;
mod protocol;
mod router;
mod supervisor;

use crate::{
    NetworkEndpoint, daemon_config::ResolvedNetwork, daemon_host::network::EngineAccess,
    peer_session::AdmittedTeam,
};
pub(crate) use control::NetworkResponse;
use iroh::{EndpointAddr, EndpointId};
pub(crate) use protocol::{ControlError, NetworkEnvelope};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Notify, RwLock, Semaphore, watch};

const DEADLINE: Duration = Duration::from_secs(10);
const MAX_CONNECTING: usize = 16;

pub(super) struct State {
    endpoint: Arc<NetworkEndpoint>,
    authority: Arc<AdmittedTeam>,
    settings: Arc<ResolvedNetwork>,
    engine: EngineAccess,
    addresses: RwLock<BTreeMap<EndpointId, EndpointAddr>>,
    wake: Notify,
    stop: watch::Sender<bool>,
    controls: Arc<Semaphore>,
    person: String,
    machine: String,
    root: PathBuf,
    codes: codes::Codes,
    connections: connections::Connections,
    connect: tokio::sync::mpsc::Sender<supervisor::Connect>,
}
#[derive(Clone)]
pub(crate) struct NetworkHandle(Arc<State>);
impl NetworkHandle {
    pub(crate) fn stopping(&self) -> watch::Receiver<bool> {
        self.0.stop.subscribe()
    }
    pub(crate) fn authority(&self) -> Arc<AdmittedTeam> {
        self.0.authority.clone()
    }
    pub(crate) async fn request(
        &self,
        connection: crate::daemon_engine::ConnectionId,
        envelope: NetworkEnvelope,
        cancelled: watch::Receiver<bool>,
    ) -> NetworkResponse {
        let id = envelope.id;
        let protocol::NetworkOperation::Network = envelope.op;
        let fence = Arc::new(crate::peer_session::AdmissionFence::new(cancelled.clone()));
        let _request_fence = StopRequest(fence.clone());
        let work = async {
            self.0
                .engine
                .authorize(connection, envelope.auth)
                .await
                .map_err(|_| ControlError::Unauthorized)?;
            let _permit = self
                .0
                .controls
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| ControlError::Busy)?;
            self.0.control(id, envelope.command, fence.clone()).await
        };
        let stopping = self.0.stop.subscribe();
        if *cancelled.borrow() || *stopping.borrow() {
            return NetworkResponse::error(id, ControlError::Rejected);
        }
        tokio::select! {
            biased;
            _ = stopped(cancelled) => NetworkResponse::error(id, ControlError::Rejected),
            _ = stopped(stopping) => NetworkResponse::error(id, ControlError::Rejected),
            result = tokio::time::timeout(Duration::from_secs(60), work) => match result {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => NetworkResponse::error(id, error),
                Err(_) => NetworkResponse::error(id, ControlError::Busy),
            }
        }
    }
}
pub(crate) struct NetworkOwner {
    stop: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<Result<(), ControlError>>>,
}
impl NetworkOwner {
    pub(crate) async fn shutdown(mut self) -> Result<(), ControlError> {
        self.stop.send_replace(true);
        self.task
            .take()
            .ok_or(ControlError::Storage)?
            .await
            .map_err(|_| ControlError::Storage)?
    }
}
impl Drop for NetworkOwner {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}
pub(crate) async fn start(
    root: PathBuf,
    engine: EngineAccess,
    settings: ResolvedNetwork,
    person: String,
    machine: String,
) -> Result<(NetworkHandle, NetworkOwner), ControlError> {
    let settings = Arc::new(settings);
    let (authority, identity) = bootstrap::open(&root, engine.clone(), settings.clone()).await?;
    let endpoint_config = settings.endpoint_config(identity.secret_key().clone());
    let relay_enabled = matches!(&endpoint_config.relay, crate::RelaySelection::Custom(_));
    let relay_only = !endpoint_config.direct_paths;
    let endpoint = Arc::new(
        NetworkEndpoint::bind(endpoint_config)
            .await
            .map_err(|_| ControlError::Transport)?,
    );
    if relay_enabled && relay_only && endpoint.online(DEADLINE).await.is_err() {
        endpoint.close().await;
        return Err(ControlError::Transport);
    }
    let (stop, _) = watch::channel(false);
    let addresses = settings.seeds.iter().map(|a| (a.id, a.clone())).collect();
    let (connect, commands) = tokio::sync::mpsc::channel(32);
    let state = Arc::new(State {
        endpoint,
        authority,
        settings,
        engine,
        addresses: RwLock::new(addresses),
        wake: Notify::new(),
        stop: stop.clone(),
        controls: Arc::new(Semaphore::new(32)),
        person,
        machine,
        root,
        codes: codes::Codes::default(),
        connections: connections::Connections::default(),
        connect,
    });
    if let Err(error) = bootstrap::startup(&state).await {
        state.stop.send_replace(true);
        state.connections.stop();
        state.endpoint.close().await;
        return Err(error);
    }
    let owner = state.clone();
    let task = tokio::spawn(async move {
        let result = supervisor::run(owner.clone(), commands).await;
        owner.stop.send_replace(true);
        owner.endpoint.close().await;
        result
    });
    Ok((
        NetworkHandle(state),
        NetworkOwner {
            stop,
            task: Some(task),
        },
    ))
}
pub(super) fn now() -> crate::peer_session::Result<i64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| crate::peer_session::PeerError::Invalid)?
            .as_millis(),
    )
    .map_err(|_| crate::peer_session::PeerError::Invalid)
}

async fn stopped(mut receiver: watch::Receiver<bool>) {
    loop {
        if *receiver.borrow_and_update() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}

impl State {
    async fn view(&self) -> Result<Option<crate::peer_session::RoutingView>, ControlError> {
        use crate::peer_session::{AuthorityStatus, PeerError};
        match self.authority.status().await {
            Ok(AuthorityStatus::Unjoined) | Err(PeerError::Quiescent) => Ok(None),
            Ok(AuthorityStatus::Active { .. }) => self
                .authority
                .routing_view()
                .await
                .map(Some)
                .map_err(|_| ControlError::Storage),
            Err(_) => Err(ControlError::Storage),
        }
    }
}

struct StopRequest(Arc<crate::peer_session::AdmissionFence>);
impl Drop for StopRequest {
    fn drop(&mut self) {
        self.0.stop_pending();
    }
}
