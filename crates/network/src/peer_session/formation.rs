use super::*;
use crate::team_state::Formation;

pub enum AuthorityStatus {
    Unjoined,
    Active {
        team: String,
        founder: EndpointId,
        revision: u64,
    },
}
impl AdmittedTeam {
    /// Open one owner for either a verified empty store or an active team. A
    /// removed, corrupt or differently bound store is never implicitly reset.
    pub fn open(
        store: FullTeamStore,
        ownership: DaemonLease,
        local: EndpointId,
        clock: impl Fn() -> Result<i64> + Send + 'static,
        mailbox: Option<TextMailbox>,
    ) -> Result<Arc<Self>> {
        let mut gate = Self::owned_gate(store, ownership, local, clock, mailbox)?;
        let now = gate.now()?;
        match gate.store.load_status(now).map_err(codes::store_error)? {
            LoadedStatus::Empty(_) => {}
            LoadedStatus::Active(loaded) => {
                let document = loaded.document().ok_or(PeerError::Unauthorized)?;
                member(document, local)?;
                gate.team = document.id.clone();
                gate.founder = document.founder;
            }
            LoadedStatus::Removed(_) => return Err(PeerError::Quiescent),
        }
        Ok(Arc::new(Self {
            local,
            gate: Arc::new(Mutex::new(gate)),
        }))
    }
    pub async fn status(&self) -> Result<AuthorityStatus> {
        let mut guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            if guard.quiescent {
                return Err(PeerError::Quiescent);
            }
            if guard.team.is_empty() {
                let now = guard.now()?;
                return match guard.store.load_status(now).map_err(codes::store_error)? {
                    LoadedStatus::Empty(_) => Ok(AuthorityStatus::Unjoined),
                    _ => {
                        guard.quiescent = true;
                        Err(PeerError::Quiescent)
                    }
                };
            }
            let (document, revision) = guard.current()?;
            Ok(AuthorityStatus::Active {
                team: document.id,
                founder: document.founder,
                revision,
            })
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
    pub async fn form(
        &self,
        name: String,
        person: String,
        machine: String,
    ) -> Result<TeamPublication> {
        self.form_with_relay(name, person, machine, None).await
    }
    pub async fn form_with_relay(
        &self,
        name: String,
        person: String,
        machine: String,
        relay: Option<String>,
    ) -> Result<TeamPublication> {
        let mut guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            if guard.quiescent || !guard.team.is_empty() {
                return Err(PeerError::Unauthorized);
            }
            let now = guard.now()?;
            let base = match guard.store.load_status(now).map_err(codes::store_error)? {
                LoadedStatus::Empty(base) => base,
                _ => return Err(PeerError::Unauthorized),
            };
            let pending = guard
                .store
                .prepare_formation_with_relay(
                    &base,
                    Formation {
                        name: &name,
                        person: &person,
                        machine: &machine,
                    },
                    relay.as_deref(),
                    now,
                )
                .map_err(codes::store_error)?;
            let loaded = guard.store.commit_formation(pending).map_err(|error| {
                guard.quiescent = true;
                codes::store_error(error)
            })?;
            let document = loaded.document().ok_or(PeerError::Unauthorized)?;
            member(document, guard.local)?;
            guard.team = document.id.clone();
            guard.founder = document.founder;
            TeamPublication::current(guard)
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
}
