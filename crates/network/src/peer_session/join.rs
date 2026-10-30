use super::*;
use crate::{
    full_team_store::{CodeJoinTrust, CodeLease, JoinTrust},
    team::{JoinRequest, TeamDocument},
    team_state::Admission,
};

/// Caller-selected invitation trust, independent of the response JSON. None
/// permits the authenticated inviter selected by the trusted invitation to
/// introduce the founder; Some additionally pins a known founder. No freshness
/// assertion is made about an otherwise valid signed snapshot.
pub struct PublicBootstrap {
    pub expected_team: String,
    pub expected_founder: Option<EndpointId>,
    pub authenticated_inviter: EndpointId,
    pub state: TeamDocument,
}

/// Opaque code endpoint authentication replaces roster-inviter trust here. The
/// owned anchor must be constructed from the selected code and ACTUAL JOIN peer,
/// never from a member/endpoint asserted in reply JSON. Optional independent team
/// and founder pins further restrict the introduced snapshot. No Debug/Serialize.
pub struct PublicCodeBootstrap {
    pub anchor: crate::code_invite::CodeAnchor,
    pub expected_team: Option<String>,
    pub expected_founder: Option<EndpointId>,
    pub state: TeamDocument,
}
enum BootstrapInput {
    Member(PublicBootstrap),
    Code(PublicCodeBootstrap),
}

/// Committed admission and its bounded reply permit. No Debug/Serialize. Keep
/// this value alive through JOIN reply serialization/write/finish, then drop it
/// BEFORE draining the old connection. Never detach a cloned document from this
/// guard and advertise it after allowing a concurrent removal or rotation.
pub struct JoinAdmission {
    _guard: OwnedMutexGuard<Gate>,
    state: TeamDocument,
    peer: EndpointId,
    revision: u64,
}
impl JoinAdmission {
    pub fn state(&self) -> &TeamDocument {
        &self.state
    }
    pub fn peer(&self) -> EndpointId {
        self.peer
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}
impl AsRef<TeamDocument> for JoinAdmission {
    fn as_ref(&self) -> &TeamDocument {
        self.state()
    }
}
impl AdmittedTeam {
    /// First durable join only. No reset/rejoin of an active or removed store.
    /// Ownership is captured before detached validation/commit work begins.
    pub fn bootstrap(
        store: FullTeamStore,
        ownership: DaemonLease,
        local: EndpointId,
        clock: impl Fn() -> Result<i64> + Send + 'static,
        mailbox: Option<TextMailbox>,
        input: PublicBootstrap,
    ) -> impl std::future::Future<Output = Result<Arc<Self>>> + Send {
        Self::bootstrap_input(
            store,
            ownership,
            local,
            clock,
            mailbox,
            BootstrapInput::Member(input),
        )
    }

    /// Empty-store bootstrap authenticated by a selected short code endpoint.
    /// The derived endpoint is not required or treated as a TEAM roster member.
    pub fn bootstrap_code(
        store: FullTeamStore,
        ownership: DaemonLease,
        local: EndpointId,
        clock: impl Fn() -> Result<i64> + Send + 'static,
        mailbox: Option<TextMailbox>,
        input: PublicCodeBootstrap,
    ) -> impl std::future::Future<Output = Result<Arc<Self>>> + Send {
        Self::bootstrap_input(
            store,
            ownership,
            local,
            clock,
            mailbox,
            BootstrapInput::Code(input),
        )
    }

    fn bootstrap_input(
        store: FullTeamStore,
        ownership: DaemonLease,
        local: EndpointId,
        clock: impl Fn() -> Result<i64> + Send + 'static,
        mailbox: Option<TextMailbox>,
        input: BootstrapInput,
    ) -> impl std::future::Future<Output = Result<Arc<Self>>> + Send {
        // Own the guard even if the returned future is dropped before first poll.
        let gate = Self::owned_gate(store, ownership, local, clock, mailbox);
        async move {
            let mut gate = gate?;
            tokio::task::spawn_blocking(move || {
                let now = gate.now()?;
                let base = gate.store.load(now).map_err(|_| PeerError::Storage)?;
                let pending = match &input {
                    BootstrapInput::Member(input) => gate.store.prepare_join(
                        &base,
                        &input.state,
                        JoinTrust {
                            team: &input.expected_team,
                            founder: input.expected_founder,
                            inviter: input.authenticated_inviter,
                        },
                        now,
                    ),
                    BootstrapInput::Code(input) => gate.store.prepare_code_join(
                        &base,
                        &input.state,
                        CodeJoinTrust {
                            anchor: &input.anchor,
                            team: input.expected_team.as_deref(),
                            founder: input.expected_founder,
                        },
                        now,
                    ),
                }
                .map_err(|_| PeerError::Invalid)?;
                let loaded = gate
                    .store
                    .commit_join(pending)
                    .map_err(|_| PeerError::Storage)?;
                let document = loaded.document().ok_or(PeerError::Unauthorized)?;
                member(document, local)?;
                gate.team = document.id.clone();
                gate.founder = document.founder;
                Ok(Arc::new(Self {
                    local,
                    gate: Arc::new(Mutex::new(gate)),
                }))
            })
            .await
            .map_err(|_| PeerError::Storage)?
        }
    }

    /// Peer is the actual authenticated JOIN remote_id, not an asserted request
    /// field. The gate remains held by the result until the transport sends it.
    pub async fn admit_join(
        &self,
        peer: EndpointId,
        request: JoinRequest,
    ) -> Result<JoinAdmission> {
        bound(&request, false)?;
        let guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || admit(guard, peer, request, None, None))
            .await
            .map_err(|_| PeerError::Storage)?
    }

    /// For a derived-code listener, bind admission to that listener's still-live
    /// intent and exact bearer. A consumed intent never authorizes a new listener
    /// or new admission; the already committed JoinAdmission drains its own reply.
    pub async fn admit_code_join(
        &self,
        peer: EndpointId,
        request: JoinRequest,
        lease: Arc<CodeLease>,
    ) -> Result<JoinAdmission> {
        self.code_admission(peer, request, lease, None).await
    }
    pub(crate) async fn admit_code_join_fenced(
        &self,
        peer: EndpointId,
        request: JoinRequest,
        lease: Arc<CodeLease>,
        fence: Arc<AdmissionFence>,
    ) -> Result<JoinAdmission> {
        self.code_admission(peer, request, lease, Some(fence)).await
    }
    async fn code_admission(
        &self,
        peer: EndpointId,
        request: JoinRequest,
        lease: Arc<CodeLease>,
        fence: Option<Arc<AdmissionFence>>,
    ) -> Result<JoinAdmission> {
        bound(&request, true)?;
        let guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            admit(guard, peer, request, Some(lease), fence.as_deref())
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
}
fn bound(request: &JoinRequest, code: bool) -> Result<()> {
    if (!code && request.team.is_empty())
        || request.team.len() > 256
        || request.secret.is_empty()
        || request.secret.len() > 4096
        || request.person.len() > 64
        || request.machine.len() > 64
        || request.proof.as_ref().is_some_and(|p| p.len() > 256 * 1024)
    {
        return Err(PeerError::Limit);
    }
    Ok(())
}
fn admit(
    mut guard: OwnedMutexGuard<Gate>,
    peer: EndpointId,
    request: JoinRequest,
    lease: Option<Arc<CodeLease>>,
    fence: Option<&AdmissionFence>,
) -> Result<JoinAdmission> {
    let (document, _) = guard.current()?;
    if (!request.team.is_empty() && request.team != document.id)
        || (request.team.is_empty() && lease.is_none())
    {
        return Err(PeerError::DifferentTeam);
    }
    if peer == guard.local {
        return Err(PeerError::Unauthorized);
    }
    let now = guard.now()?;
    if let Some(lease) = lease {
        let derived = guard
            .store
            .validate_code_lease(&lease, now)
            .map_err(super::codes::store_error)?;
        if !same_secret(derived.bearer_secret(), &request.secret) {
            return Err(PeerError::Unauthorized);
        }
    }
    let base = guard.store.load(now).map_err(|_| PeerError::Storage)?;
    let pending = guard
        .store
        .prepare_admission(
            &base,
            Admission {
                endpoint: peer,
                person: &request.person,
                machine: &request.machine,
                secret: &request.secret,
                proof: request.proof.as_deref(),
                owner_only: request.owner_only,
            },
            now,
        )
        .map_err(|_| PeerError::Unauthorized)?;
    if fence.is_some_and(|fence| !fence.begin_commit()) {
        return Err(PeerError::Closed);
    }
    if pending.requires_quiescence() {
        guard.quiescent = true;
    }
    guard.store.commit_transition(pending, now).map_err(|_| {
        guard.quiescent = true;
        PeerError::Storage
    })?;
    let (state, revision) = guard.current()?;
    member(&state, peer)?;
    Ok(JoinAdmission {
        _guard: guard,
        state,
        peer,
        revision,
    })
}
fn same_secret(expected: &str, offered: &str) -> bool {
    let expected = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, expected.as_bytes());
    let offered = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, offered.as_bytes());
    let tag = ring::hmac::sign(&offered, b"peer join bearer");
    ring::hmac::verify(&expected, b"peer join bearer", tag.as_ref()).is_ok()
}
