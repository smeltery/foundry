use super::*;
use crate::full_team_store::{CodeJoinTrust, JoinTrust};

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
                gate.bootstrap_commit(input, None)?;
                Ok(Arc::new(Self {
                    local,
                    gate: Arc::new(Mutex::new(gate)),
                }))
            })
            .await
            .map_err(|_| PeerError::Storage)?
        }
    }

    pub async fn join_existing(&self, input: PublicBootstrap) -> Result<()> {
        self.join_input(BootstrapInput::Member(input), None).await
    }
    pub async fn join_code_existing(&self, input: PublicCodeBootstrap) -> Result<()> {
        self.join_input(BootstrapInput::Code(input), None).await
    }
    pub async fn join_existing_fenced(
        &self,
        input: PublicBootstrap,
        fence: Arc<AdmissionFence>,
    ) -> Result<()> {
        self.join_input(BootstrapInput::Member(input), Some(fence))
            .await
    }
    pub async fn join_code_existing_fenced(
        &self,
        input: PublicCodeBootstrap,
        fence: Arc<AdmissionFence>,
    ) -> Result<()> {
        self.join_input(BootstrapInput::Code(input), Some(fence))
            .await
    }
    async fn join_input(
        &self,
        input: BootstrapInput,
        fence: Option<Arc<AdmissionFence>>,
    ) -> Result<()> {
        let _stop = fence
            .as_ref()
            .map(|fence| super::super::admission_fence::StopAdmission(fence.clone()));
        let mut guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || guard.bootstrap_commit(input, fence.as_deref()))
            .await
            .map_err(|_| PeerError::Storage)?
    }
}
impl Gate {
    fn bootstrap_commit(
        &mut self,
        input: BootstrapInput,
        fence: Option<&AdmissionFence>,
    ) -> Result<()> {
        if self.quiescent || !self.team.is_empty() {
            return Err(PeerError::Unauthorized);
        }
        let now = self.now()?;
        let base = self.store.load(now).map_err(|_| PeerError::Storage)?;
        let pending = match &input {
            BootstrapInput::Member(input) => self.store.prepare_join(
                &base,
                &input.state,
                JoinTrust {
                    team: &input.expected_team,
                    founder: input.expected_founder,
                    inviter: input.authenticated_inviter,
                },
                now,
            ),
            BootstrapInput::Code(input) => self.store.prepare_code_join(
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
        let loaded = self
            .store
            .commit_join_when(pending, || fence.is_none_or(AdmissionFence::begin_commit))
            .map_err(|error| {
                if error == crate::full_team_store::StoreError::Cancelled {
                    PeerError::Closed
                } else {
                    self.quiescent = true;
                    PeerError::Storage
                }
            })?;
        let document = loaded.document().ok_or(PeerError::Unauthorized)?;
        member(document, self.local)?;
        self.team = document.id.clone();
        self.founder = document.founder;
        Ok(())
    }
}
