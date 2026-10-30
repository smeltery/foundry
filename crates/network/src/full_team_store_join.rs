use super::*;
use iroh::EndpointId;

/// Team and inviter are selected independently of reply JSON. A pinned founder
/// restricts the root; otherwise the authenticated inviter introduces that root.
/// Inviter comes from the actual JOIN connection and the trusted invitation.
pub struct JoinTrust<'a> {
    pub team: &'a str,
    pub founder: Option<EndpointId>,
    pub inviter: EndpointId,
}

/// A short code authenticates a derived listener, not an admitted team member.
/// The reply must contain that exact invitation consumed by this local key.
pub struct CodeJoinTrust<'a> {
    pub anchor: &'a crate::code_invite::CodeAnchor,
    pub team: Option<&'a str>,
    pub founder: Option<EndpointId>,
}

/// Verified candidate for an empty store; no invitation material is exposed.
/// ```compile_fail
/// fn diagnostic<T: std::fmt::Debug>() {}
/// diagnostic::<foundry_network::full_team_store::PendingJoin>();
/// ```
pub struct PendingJoin(PendingFormation);

impl FullTeamStore {
    /// Validate every signed record without dropping unsupported fields or
    /// replaying new-member name rules over historical accepted membership.
    /// This proves a rooted snapshot and local admission, not remote freshness.
    pub fn prepare_join(
        &self,
        base: &LoadedTeam,
        incoming: &TeamDocument,
        trust: JoinTrust<'_>,
        now: i64,
    ) -> Result<PendingJoin> {
        if trust.team.is_empty() || trust.inviter == self.identity.endpoint_id() {
            return Err(StoreError::Binding);
        }
        let pending =
            self.prepare_join_snapshot(base, incoming, Some(trust.team), trust.founder, now)?;
        if !incoming.admissions.contains_key(&trust.inviter.to_string())
            || !incoming
                .members
                .get(&trust.inviter.to_string())
                .is_some_and(|m| m.body.active)
        {
            return Err(StoreError::Binding);
        }
        Ok(pending)
    }

    pub fn prepare_code_join(
        &self,
        base: &LoadedTeam,
        incoming: &TeamDocument,
        trust: CodeJoinTrust<'_>,
        now: i64,
    ) -> Result<PendingJoin> {
        if !trust.anchor.matches_profile(&self.policy.profile) {
            return Err(StoreError::Binding);
        }
        let pending = self.prepare_join_snapshot(base, incoming, trust.team, trust.founder, now)?;
        let invitation = incoming
            .invites
            .get(trust.anchor.invitation_hash())
            .ok_or(StoreError::Binding)?;
        if invitation.body.reusable || invitation.body.used_by != Some(self.identity.endpoint_id())
        {
            return Err(StoreError::Binding);
        }
        Ok(pending)
    }

    fn prepare_join_snapshot(
        &self,
        base: &LoadedTeam,
        incoming: &TeamDocument,
        team: Option<&str>,
        founder: Option<EndpointId>,
        now: i64,
    ) -> Result<PendingJoin> {
        self.protection.check()?;
        if now < 0 {
            return Err(StoreError::Invalid);
        }
        if !Arc::ptr_eq(&self.instance, &base.instance) {
            return Err(StoreError::Binding);
        }
        if base.revision() != 0 || base.document().is_some() {
            return Err(StoreError::Conflict);
        }
        if team.is_some_and(|team| team.is_empty() || incoming.id != team)
            || founder.is_some_and(|founder| incoming.founder != founder)
        {
            return Err(StoreError::Binding);
        }
        input::bound(incoming)?;
        let mut checkpoint = Checkpoint::empty(
            self.instance.incarnation,
            self.identity.endpoint_id(),
            self.policy.digest,
        );
        checkpoint.configure(&self.policy);
        checkpoint.revision = 1;
        checkpoint.parent = base.digest;
        checkpoint.accepted_ms = now;
        checkpoint.operation_ms = now;
        checkpoint.network_generation = u64::from(self.policy.codes.is_some());
        checkpoint.state = StoredState::Active {
            document: Box::new(incoming.clone()),
        };
        validation::validate(&checkpoint, &self.policy, now)?;
        checkpoint::encode(&checkpoint)?;
        Ok(PendingJoin(PendingFormation {
            instance: self.instance.clone(),
            base: base.digest,
            checkpoint,
        }))
    }

    /// Exact empty-store CAS and sync complete before the caller advertises its
    /// membership. A lost result requires reload rather than an implicit reset.
    pub fn commit_join(&mut self, pending: PendingJoin) -> Result<LoadedTeam> {
        self.commit_formation(pending.0)
    }
}
