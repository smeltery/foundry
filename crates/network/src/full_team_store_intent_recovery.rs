use super::*;
impl FullTeamStore {
    /// Recovery reads authenticated committed state. Consumed/expired codes never
    /// authorize a new listener, even if an ordinary long-ticket retry can succeed.
    pub fn recover_code_intents(&mut self, now: i64) -> Result<Vec<CodeLease>> {
        if self.policy.codes.is_none() {
            return Err(StoreError::Unsupported);
        }
        if now < 0 {
            return Err(StoreError::Invalid);
        }
        let state = match self.load_status(now)? {
            LoadedStatus::Active(state) => state,
            LoadedStatus::Empty(_) | LoadedStatus::Removed(_) => return Ok(Vec::new()),
        };
        let document = state.document().ok_or(StoreError::Corrupt)?;
        Ok(state
            .checkpoint
            .code_intents
            .iter()
            .filter(|i| intents::eligible(i, document, now))
            .map(|i| CodeLease {
                instance: self.instance.clone(),
                id: i.id,
                generation: state.checkpoint.network_generation,
                expires_ms: i.expires_ms,
            })
            .collect())
    }
    /// Fresh database read, not a cached snapshot. Host must serialize this check
    /// with revocation before bind and again before publishing a completed bind.
    /// No database transaction spans external effects. Existing consumed join-reply
    /// draining requires a separate host-owned connection lease, never this token.
    pub fn validate_code_lease(
        &mut self,
        lease: &CodeLease,
        now: i64,
    ) -> Result<crate::code_invite::DerivedCode> {
        if !Arc::ptr_eq(&self.instance, &lease.instance) {
            return Err(StoreError::Binding);
        }
        if now < 0 {
            return Err(StoreError::Invalid);
        }
        let state = self.load(now)?;
        if state.checkpoint.network_generation != lease.generation {
            return Err(StoreError::Conflict);
        }
        let intent = state
            .checkpoint
            .code_intents
            .iter()
            .find(|i| i.id == lease.id)
            .ok_or(StoreError::Conflict)?;
        if !intents::eligible(intent, state.document().ok_or(StoreError::Conflict)?, now) {
            return Err(StoreError::Conflict);
        }
        let policy = self.policy.codes.as_ref().ok_or(StoreError::Unsupported)?;
        let code = policy
            .normalize(&intent.code)
            .map_err(|_| StoreError::Corrupt)?;
        policy.derive(&code).map_err(|_| StoreError::Corrupt)
    }
    /// Descriptors contain no captured wire state or endpoint material. Always
    /// render publications from fresh committed state under the host's write gate.
    pub fn code_effects(&mut self, now: i64) -> Result<Vec<ControlEffect>> {
        if now < 0 {
            return Err(StoreError::Invalid);
        }
        if self.policy.codes.is_none() {
            return Err(StoreError::Unsupported);
        }
        match self.load_status(now)? {
            LoadedStatus::Active(state) => Ok(state.checkpoint.effects.clone()),
            _ => Ok(Vec::new()),
        }
    }
    /// Exact descriptor acknowledgement advances revision even without a document
    /// mutation. A stale CAS cannot delete a newer generation's desired effect.
    pub fn prepare_effect_ack(
        &self,
        base: &LoadedTeam,
        effect: &ControlEffect,
        now: i64,
    ) -> Result<PendingTransition> {
        if now < 0 {
            return Err(StoreError::Invalid);
        }
        self.engine(base, now)?;
        if self.policy.codes.is_none() {
            return Err(StoreError::Unsupported);
        }
        let index = base
            .checkpoint
            .effects
            .iter()
            .position(|e| e == effect)
            .ok_or(StoreError::Conflict)?;
        let mut checkpoint = Checkpoint::empty(
            self.instance.incarnation,
            self.identity.endpoint_id(),
            self.policy.digest,
        );
        checkpoint.configure(&self.policy);
        checkpoint.state = StoredState::Active {
            document: Box::new(base.document().ok_or(StoreError::Conflict)?.clone()),
        };
        checkpoint.code_intents = base.checkpoint.code_intents.clone();
        checkpoint.effects = base.checkpoint.effects.clone();
        checkpoint.effects.remove(index);
        checkpoint.network_generation = base.checkpoint.network_generation;
        checkpoint.parent = base.digest;
        checkpoint.revision = base
            .revision()
            .checked_add(1)
            .ok_or(StoreError::RevisionOverflow)?;
        if checkpoint.revision > i64::MAX as u64 {
            return Err(StoreError::RevisionOverflow);
        }
        checkpoint.accepted_ms = base.checkpoint.accepted_ms.max(now);
        checkpoint.operation_ms = now;
        validation::validate(&checkpoint, &self.policy, now)?;
        checkpoint::encode(&checkpoint)?;
        Ok(PendingTransition {
            instance: self.instance.clone(),
            base: base.digest,
            revision: base.revision(),
            checkpoint,
            changed: true,
        })
    }
}
