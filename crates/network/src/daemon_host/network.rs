use super::*;
use crate::{
    daemon_engine::{Auth, Reply},
    peer_session::{AdmittedTeam, PeerError},
};

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;

fn sync_members(
    engine: &mut Engine,
    view: &crate::peer_session::RoutingView,
) -> Result<(), EngineError> {
    let members = view
        .members
        .iter()
        .filter(|member| member.active && member.endpoint != view.local)
        .map(|member| crate::daemon_engine::RemoteMember {
            peer: member.endpoint,
            person: member.person.clone(),
            machine: member.machine.clone(),
        })
        .collect::<Vec<_>>();
    engine.sync_remote_members(&view.team, view.revision, &members)
}

/// Every callback and detached operation retains the concrete daemon owner.
#[derive(Clone)]
pub(crate) struct EngineAccess {
    owner: Arc<OwnedEngine>,
}
impl EngineAccess {
    pub(super) fn new(owner: Arc<OwnedEngine>) -> Self {
        Self { owner }
    }
    pub(crate) fn directory(&self) -> Result<Dir, HostError> {
        self.owner.root.try_clone().map_err(|_| HostError::Storage)
    }
    pub(crate) fn lease(&self) -> Result<DaemonLease, HostError> {
        self.owner._lease.retain().map_err(|_| HostError::Storage)
    }
    pub(crate) async fn with<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Engine) -> Result<T, EngineError> + Send + 'static,
    ) -> Result<T, EngineError> {
        let owner = self.owner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = owner
                .inner
                .lock()
                .map_err(|_| EngineError::StorageUncertain)?;
            operation(&mut engine)
        })
        .await
        .map_err(|_| EngineError::StorageUncertain)?
    }
    pub(crate) async fn authorize(
        &self,
        connection: ConnectionId,
        auth: Auth,
    ) -> Result<(), EngineError> {
        self.with(move |engine| engine.authorize_control(connection, &auth))
            .await
    }
    pub(crate) fn recipient(&self, address: &str) -> Result<String, PeerError> {
        self.owner
            .inner
            .lock()
            .map_err(|_| PeerError::Storage)?
            .resolve_incoming_recipient(address)
            .map_err(|_| PeerError::Unauthorized)
    }
    pub(crate) fn notification(&self) -> Result<Arc<tokio::sync::Notify>, HostError> {
        Ok(self
            .owner
            .inner
            .lock()
            .map_err(|_| HostError::Storage)?
            .arrival_notification())
    }
    pub(crate) async fn dispatch(
        &self,
        connection: ConnectionId,
        request: RequestEnvelope,
        authority: Option<Arc<AdmittedTeam>>,
    ) -> Result<Reply, HostError> {
        let view = match authority {
            Some(team) => match team.status().await {
                Ok(crate::peer_session::AuthorityStatus::Unjoined) | Err(PeerError::Quiescent) => {
                    None
                }
                Ok(crate::peer_session::AuthorityStatus::Active { .. }) => {
                    Some(team.routing_view().await.map_err(|_| HostError::Storage)?)
                }
                Err(_) => return Err(HostError::Storage),
            },
            None => None,
        };
        self.with(move |engine| {
            // The view retains the actual membership gate in this blocking
            // closure; reverse callbacks acquire authority before Engine too.
            if let Some(view) = &view {
                sync_members(engine, view)?;
            } else {
                engine.suspend_remote_directory();
            }
            Ok(engine.handle(connection, request))
        })
        .await
        .map_err(|_| HostError::Storage)
    }
}

impl EngineAccess {
    pub(crate) async fn sync(
        &self,
        view: crate::peer_session::RoutingView,
        presence: Option<(iroh::EndpointId, u64, crate::wire::Presence)>,
    ) -> Result<(), crate::daemon_network::ControlError> {
        self.with(move |engine| {
            // Borrow the whole guarded value inside detached blocking work.
            sync_members(engine, &view)?;
            if let Some((peer, generation, presence)) = presence {
                engine.remote_presence(peer, generation, &presence)?;
            }
            Ok(())
        })
        .await
        .map_err(|_| crate::daemon_network::ControlError::Storage)
    }
}

#[cfg(test)]
impl EngineAccess {
    pub(crate) fn for_test(daemon: &Daemon) -> Self {
        Self::new(daemon.engine.clone())
    }
}
