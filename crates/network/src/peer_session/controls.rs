use super::*;
use crate::full_team_store::{PendingTransition, ScrubStatus};

/// Privileged committed output. Retain through one bounded intentional sink;
/// never log it or detach a cloned state after releasing this guard.
pub struct TeamPublication {
    guard: OwnedMutexGuard<Gate>,
    state: TeamDocument,
}
impl TeamPublication {
    pub(super) fn current(mut guard: OwnedMutexGuard<Gate>) -> Result<Self> {
        let (state, _) = guard.current()?;
        Ok(Self { guard, state })
    }
    pub fn state(&self) -> &TeamDocument {
        &self.state
    }
    pub fn profile(&self) -> &WireProfile {
        self.guard.store.wire_profile()
    }
}
impl AsRef<TeamDocument> for TeamPublication {
    fn as_ref(&self) -> &TeamDocument {
        self.state()
    }
}
enum Change {
    Invite {
        secret: String,
        hours: u64,
        own: bool,
    },
    Rotation,
    Proof {
        endpoint: EndpointId,
        token: String,
    },
    Relay(Option<String>),
}
impl Gate {
    fn commit_control(&mut self, pending: PendingTransition, now: i64) -> Result<CommitOutcome> {
        if pending.requires_quiescence() {
            self.quiescent = true;
        }
        self.store.commit_transition(pending, now).map_err(|error| {
            self.quiescent = true;
            codes::store_error(error)
        })
    }
}
impl AdmittedTeam {
    pub async fn team_publication(&self) -> Result<TeamPublication> {
        let mut guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let (state, _) = guard.current()?;
            Ok(TeamPublication { guard, state })
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
    pub async fn issue_invite(
        &self,
        secret: String,
        hours: u64,
        own: bool,
    ) -> Result<TeamPublication> {
        self.change(Change::Invite { secret, hours, own }).await
    }
    pub async fn rotate_link(&self) -> Result<TeamPublication> {
        self.change(Change::Rotation).await
    }
    pub async fn attach_proof(
        &self,
        endpoint: EndpointId,
        token: String,
    ) -> Result<TeamPublication> {
        self.change(Change::Proof { endpoint, token }).await
    }
    /// Commit relay metadata only. The runtime owns actual transport validation
    /// and restart; no network operation runs while a store transaction is open.
    pub async fn set_relay(&self, relay: Option<String>) -> Result<TeamPublication> {
        self.change(Change::Relay(relay)).await
    }
    async fn change(&self, change: Change) -> Result<TeamPublication> {
        let mut guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            guard.current()?;
            let now = guard.now()?;
            let base = guard.store.load(now).map_err(codes::store_error)?;
            let pending = match &change {
                Change::Invite { secret, hours, own } => {
                    guard.store.prepare_invite(&base, secret, *hours, *own, now)
                }
                Change::Rotation => guard.store.prepare_rotation(&base, now),
                Change::Proof { endpoint, token } => guard
                    .store
                    .prepare_attach_proof(&base, *endpoint, token, now),
                Change::Relay(relay) => guard.store.prepare_relay(&base, relay.as_deref(), now),
            }
            .map_err(codes::store_error)?;
            guard.commit_control(pending, now)?;
            let (state, _) = guard.current()?;
            Ok(TeamPublication { guard, state })
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
    pub async fn depart(&self) -> Result<ScrubStatus> {
        let mut guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            guard.current()?;
            let now = guard.now()?;
            let base = guard.store.load(now).map_err(codes::store_error)?;
            let pending = guard
                .store
                .prepare_departure(&base, now)
                .map_err(codes::store_error)?;
            match guard.commit_control(pending, now)? {
                CommitOutcome::Removed { scrub, .. } => Ok(scrub),
                _ => {
                    guard.quiescent = true;
                    Err(PeerError::Storage)
                }
            }
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
}
