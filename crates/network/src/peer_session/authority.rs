use super::*;
use crate::daemon_lease::DaemonLease;
use crate::{
    full_team_store::{CommitOutcome, FullTeamStore, LoadedStatus},
    signing::MemberRecord,
    team::TeamDocument,
};
use tokio::sync::{Mutex, OwnedMutexGuard};
#[path = "codes.rs"]
mod codes;
#[path = "join.rs"]
mod join;
pub use codes::{CodePermit, IssuedInvitation};
pub use join::{JoinAdmission, PublicBootstrap, PublicCodeBootstrap};

/// One shared authorization/writer gate per local team. All cooperating local
/// membership mutations must use this gate, or quiesce sessions before external
/// store writes. Reload checks observe other store handles at the next boundary;
/// bytes handed to QUIC before a removal cannot be recalled. No DB transaction is
/// held over I/O. A bounded writer permit serializes with local remove/refresh.
/// The consumed ownership guard lives inside Gate, including detached blocking
/// work after async cancellation. Constructor verifies the protected store root
/// and actual local key against that guard before loading authority.
pub struct AdmittedTeam {
    local: EndpointId,
    gate: Arc<Mutex<Gate>>,
}
pub(super) struct Gate {
    store: FullTeamStore,
    local: EndpointId,
    team: String,
    founder: EndpointId,
    clock: Box<dyn Fn() -> Result<i64> + Send>,
    quiescent: bool,
    mailbox: Option<TextMailbox>,
    // Drop the stores before the last retained ownership guard.
    _lease: DaemonLease,
}
pub(super) struct Permit {
    pub guard: OwnedMutexGuard<Gate>,
    pub document: TeamDocument,
    pub revision: u64,
    pub local: MemberRecord,
    pub remote: MemberRecord,
}
impl AdmittedTeam {
    /// The supplied store must already be durably joined. Trust/roots/profile are
    /// chosen when constructing its StorePolicy, never supplied by peer JSON.
    pub fn new(
        store: FullTeamStore,
        ownership: DaemonLease,
        local: EndpointId,
        clock: impl Fn() -> Result<i64> + Send + 'static,
        mailbox: Option<TextMailbox>,
    ) -> Result<Arc<Self>> {
        let mut gate = Self::owned_gate(store, ownership, local, clock, mailbox)?;
        let now = gate.now()?;
        let loaded = gate.store.load(now).map_err(|_| PeerError::Storage)?;
        let document = loaded.document().ok_or(PeerError::Unauthorized)?;
        member(document, local)?;
        gate.team = document.id.clone();
        gate.founder = document.founder;
        Ok(Arc::new(Self {
            local,
            gate: Arc::new(Mutex::new(gate)),
        }))
    }

    fn owned_gate(
        store: FullTeamStore,
        ownership: DaemonLease,
        local: EndpointId,
        clock: impl Fn() -> Result<i64> + Send + 'static,
        mailbox: Option<TextMailbox>,
    ) -> Result<Gate> {
        // Field drop order also retains ownership on initialization failure.
        let gate = Gate {
            store,
            local,
            team: String::new(),
            founder: local,
            clock: Box::new(clock),
            quiescent: false,
            mailbox,
            _lease: ownership,
        };
        gate.store
            .verify_owner(&gate._lease, local)
            .map_err(|_| PeerError::Unauthorized)?;
        Ok(gate)
    }

    pub fn local(&self) -> EndpointId {
        self.local
    }
    pub(super) async fn authorize(&self, peer: EndpointId) -> Result<Permit> {
        let mut guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let (document, revision) = guard.current()?;
            let local = member(&document, guard.local)?;
            let remote = member(&document, peer)?;
            Ok(Permit {
                guard,
                document,
                revision,
                local,
                remote,
            })
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
    pub(super) async fn merge(&self, peer: EndpointId, incoming: TeamDocument) -> Result<u64> {
        let mut permit = self.authorize(peer).await?;
        tokio::task::spawn_blocking(move || {
            let gate = &mut permit.guard;
            let now = gate.now()?;
            let base = gate.store.load(now).map_err(|_| PeerError::Storage)?;
            let (pending, report) = gate
                .store
                .prepare_merge(&base, peer, &incoming, now)
                .map_err(|_| PeerError::Invalid)?;
            if pending.requires_quiescence() {
                gate.quiescent = true;
            }
            if report.rejected != 0 || report.proof_required != 0 {
                return Err(PeerError::Invalid);
            }
            let outcome = gate.store.commit_transition(pending, now).map_err(|_| {
                gate.quiescent = true;
                PeerError::Storage
            })?;
            let revision = match outcome {
                CommitOutcome::Active(state) | CommitOutcome::Unchanged(state) => state.revision(),
                CommitOutcome::Removed { .. } => return Err(PeerError::Quiescent),
            };
            // Incoming state may remove its own sender. Never advertise again on
            // this session merely because it was admitted before the merge.
            let (document, _) = gate.current()?;
            member(&document, peer)?;
            Ok(revision)
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
    /// Removal and writer authorization share a gate. Quiescence latches BEFORE
    /// a self-removal commit attempt, and never clears on uncertain persistence.
    pub async fn remove(&self, targets: Vec<EndpointId>) -> Result<()> {
        let mut guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            guard.current()?;
            let now = guard.now()?;
            let base = guard.store.load(now).map_err(|_| PeerError::Storage)?;
            let pending = guard
                .store
                .prepare_remove(&base, &targets, now)
                .map_err(|_| PeerError::Invalid)?;
            if pending.requires_quiescence() {
                guard.quiescent = true;
            }
            guard.store.commit_transition(pending, now).map_err(|_| {
                guard.quiescent = true;
                PeerError::Storage
            })?;
            Ok(())
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
    /// Force current durable local-admission validation. A removed or broken
    /// store permanently fences this gate; create a new authority after recovery.
    pub async fn refresh(&self) -> Result<u64> {
        let mut guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || guard.current().map(|(_, revision)| revision))
            .await
            .map_err(|_| PeerError::Storage)?
    }
    pub(super) async fn receive_text(
        &self,
        peer: EndpointId,
        message: wire::Message,
    ) -> Result<(String, bool)> {
        let mut permit = self.authorize(peer).await?;
        tokio::task::spawn_blocking(move || {
            frames::message(&message, &permit.remote, &permit.local)?;
            let gate = &mut permit.guard;
            let mailbox = gate.mailbox.as_mut().ok_or(PeerError::Unsupported)?;
            let id = message.id.clone();
            match mailbox.receive(&message) {
                Ok(duplicate) => Ok((id, duplicate)),
                Err(error) => {
                    if error == PeerError::Storage {
                        gate.quiescent = true;
                    }
                    Err(error)
                }
            }
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
}
impl Gate {
    fn now(&self) -> Result<i64> {
        let now = (self.clock)()?;
        if now < 0 {
            return Err(PeerError::Invalid);
        }
        Ok(now)
    }
    fn current(&mut self) -> Result<(TeamDocument, u64)> {
        if self.quiescent {
            return Err(PeerError::Quiescent);
        }
        let now = self.now()?;
        let loaded = self.store.load_status(now).map_err(|_| {
            self.quiescent = true;
            PeerError::Storage
        })?;
        let LoadedStatus::Active(state) = loaded else {
            self.quiescent = true;
            return Err(PeerError::Quiescent);
        };
        let document = state.document().ok_or(PeerError::Unauthorized)?;
        if document.id != self.team
            || document.founder != self.founder
            || member(document, self.local).is_err()
        {
            self.quiescent = true;
            return Err(PeerError::Quiescent);
        }
        Ok((document.clone(), state.revision()))
    }
}
fn member(document: &TeamDocument, endpoint: EndpointId) -> Result<MemberRecord> {
    // FullTeamStore validates every admission's rooted ancestry on every load.
    // An active member alone (without its durable admission) is insufficient.
    let id = endpoint.to_string();
    if !document.admissions.contains_key(&id) {
        return Err(PeerError::Unauthorized);
    }
    document
        .members
        .get(&id)
        .filter(|r| r.body.active && r.body.endpoint == endpoint)
        .map(|r| r.body.clone())
        .ok_or(PeerError::Unauthorized)
}

impl PeerSession {
    pub(super) async fn permit(&mut self) -> Result<Permit> {
        io::bounded(
            &mut self.options.cancel,
            self.options.deadline,
            self.team.authorize(self.peer),
        )
        .await?
    }
    pub(super) async fn merge_state(&mut self, state: TeamDocument) -> Result<u64> {
        io::bounded(
            &mut self.options.cancel,
            self.options.deadline,
            self.team.merge(self.peer, state),
        )
        .await?
    }
}
