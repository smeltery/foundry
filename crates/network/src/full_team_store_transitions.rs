use super::*;
use crate::{membership::MergeReport, team_state::Admission};
use iroh::EndpointId;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrubStatus {
    Complete,
    PendingBusy,
    Failed(ScrubFailure),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrubFailure {
    Storage,
    Protection,
    Sync,
    UnexpectedResult,
}
pub struct RemovalReceipt {
    pub(super) instance: Arc<StoreInstance>,
    pub(super) revision: u64,
    pub(super) digest: [u8; 32],
}
impl RemovalReceipt {
    pub fn revision(&self) -> u64 {
        self.revision
    }
}
pub enum LoadedStatus {
    Empty(LoadedTeam),
    Active(LoadedTeam),
    Removed(RemovalReceipt),
}
pub enum CommitOutcome {
    Unchanged(LoadedTeam),
    Active(LoadedTeam),
    Removed {
        receipt: RemovalReceipt,
        scrub: ScrubStatus,
    },
}
pub struct PendingTransition {
    instance: Arc<StoreInstance>,
    base: [u8; 32],
    revision: u64,
    checkpoint: Checkpoint,
    changed: bool,
}
impl PendingTransition {
    /// Latch before commit: validated local removal requires quiescence even if
    /// persistence fails. This is not evidence that removal has committed.
    /// At revision exhaustion a removal candidate still signals quiescence, but
    /// commit rejects it with RevisionOverflow before any write.
    pub fn requires_quiescence(&self) -> bool {
        matches!(self.checkpoint.state, StoredState::Removed { .. })
    }
}
impl FullTeamStore {
    pub(super) fn sync_committed(&self) -> Result<()> {
        #[cfg(test)]
        if self.fail_sync {
            return Err(StoreError::Indeterminate);
        }
        self.protection
            .sync()
            .map_err(|_| StoreError::Indeterminate)
    }

    fn engine(&self, base: &LoadedTeam, now: i64) -> Result<TeamState> {
        self.protection.check()?;
        if !Arc::ptr_eq(&self.instance, &base.instance) {
            return Err(StoreError::Binding);
        }
        validation::validate(&base.checkpoint, &self.policy, now)?;
        let document = base.document().ok_or(StoreError::Conflict)?;
        TeamState::restore_verified(&VerifiedLocalSnapshot {
            document,
            policy: &self.policy,
        })
        .map_err(|_| StoreError::Corrupt)
    }
    fn candidate(
        &self,
        base: &LoadedTeam,
        state: TeamState,
        now: i64,
    ) -> Result<PendingTransition> {
        let mut checkpoint = Checkpoint::empty(
            self.instance.incarnation,
            self.identity.endpoint_id(),
            self.policy.digest,
        );
        checkpoint.parent = base.digest;
        checkpoint.operation_ms = now;
        checkpoint.accepted_ms = base.checkpoint.accepted_ms.max(now);
        let changed = Some(state.document()) != base.document();
        checkpoint.revision = if changed {
            base.revision()
                .checked_add(1)
                .ok_or(StoreError::RevisionOverflow)?
        } else {
            base.revision()
        };
        // Decide removal before the first full candidate encoding/signing call.
        checkpoint.state = if state.is_active(self.identity.endpoint_id()) {
            StoredState::Active {
                document: Box::new(state.document().clone()),
            }
        } else {
            StoredState::Removed {
                team: state.document().id.clone(),
                founder: state.document().founder,
            }
        };
        if checkpoint.revision > i64::MAX as u64 {
            // Retain a private, nonpublishable removal candidate so the caller
            // can latch quiescence. Never encode, wrap or reuse the old revision.
            if !matches!(checkpoint.state, StoredState::Removed { .. }) {
                return Err(StoreError::RevisionOverflow);
            }
        } else {
            validation::validate(&checkpoint, &self.policy, now)?;
            checkpoint::encode(&checkpoint)?;
        }
        Ok(PendingTransition {
            instance: self.instance.clone(),
            base: base.digest,
            revision: base.revision(),
            checkpoint,
            changed,
        })
    }
    pub fn prepare_invite(
        &self,
        base: &LoadedTeam,
        secret: &str,
        hours: u64,
        own: bool,
        now: i64,
    ) -> Result<PendingTransition> {
        if secret.len() > MAX_CHECKPOINT_BYTES {
            return Err(StoreError::TooLarge);
        }
        let state = self
            .engine(base, now)?
            .issue_invite(self.identity.secret_key(), secret, hours, own, now)
            .map_err(|_| StoreError::Invalid)?;
        self.candidate(base, state, now)
    }
    pub fn prepare_rotation(&self, base: &LoadedTeam, now: i64) -> Result<PendingTransition> {
        let state = self
            .engine(base, now)?
            .rotate_link(self.identity.secret_key(), now, &mut SystemEntropy)
            .map_err(|_| StoreError::Invalid)?;
        self.candidate(base, state, now)
    }
    pub fn prepare_admission(
        &self,
        base: &LoadedTeam,
        request: Admission<'_>,
        now: i64,
    ) -> Result<PendingTransition> {
        if request.secret.len() > MAX_CHECKPOINT_BYTES {
            return Err(StoreError::TooLarge);
        }
        let state = self
            .engine(base, now)?
            .admit(self.identity.secret_key(), request, now)
            .map_err(|_| StoreError::Invalid)?;
        self.candidate(base, state, now)
    }
    pub fn prepare_attach_proof(
        &self,
        base: &LoadedTeam,
        endpoint: EndpointId,
        token: &str,
        now: i64,
    ) -> Result<PendingTransition> {
        let state = self
            .engine(base, now)?
            .attach_proof(endpoint, token, now)
            .map_err(|_| StoreError::Invalid)?;
        self.candidate(base, state, now)
    }
    pub fn prepare_remove(
        &self,
        base: &LoadedTeam,
        targets: &[EndpointId],
        now: i64,
    ) -> Result<PendingTransition> {
        if targets.len() > 4096 {
            return Err(StoreError::TooLarge);
        }
        let state = self
            .engine(base, now)?
            .remove(self.identity.secret_key(), targets, now, &mut SystemEntropy)
            .map_err(|_| StoreError::Invalid)?;
        self.candidate(base, state, now)
    }
    pub fn prepare_departure(&self, base: &LoadedTeam, now: i64) -> Result<PendingTransition> {
        let state = self
            .engine(base, now)?
            .departure(self.identity.secret_key(), now)
            .map_err(|_| StoreError::Invalid)?;
        self.candidate(base, state, now)
    }
    pub fn prepare_merge(
        &self,
        base: &LoadedTeam,
        peer: EndpointId,
        incoming: &TeamDocument,
        now: i64,
    ) -> Result<(PendingTransition, MergeReport)> {
        super::input::bound(incoming)?;
        let (state, report) = self
            .engine(base, now)?
            .merge_for_local(peer, incoming, now, self.identity.endpoint_id())
            .map_err(|_| StoreError::Invalid)?;
        Ok((self.candidate(base, state, now)?, report))
    }
    pub fn commit_transition(
        &mut self,
        pending: PendingTransition,
        now: i64,
    ) -> Result<CommitOutcome> {
        if !Arc::ptr_eq(&self.instance, &pending.instance) {
            return Err(StoreError::Binding);
        }
        if pending.checkpoint.revision > i64::MAX as u64 {
            return Err(StoreError::RevisionOverflow);
        }
        let guard = self.lock.lock(self.timeout)?;
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (base, digest) = checkpoint::read(&tx, &self.identity, &self.policy)?;
        if base.incarnation != self.instance.incarnation
            || digest != pending.base
            || base.revision != pending.revision
            || base.document().is_none()
        {
            return Err(StoreError::Conflict);
        }
        validation::validate(&base, &self.policy, now)?;
        validation::validate(&pending.checkpoint, &self.policy, now)?;
        if !pending.changed {
            self.protection.check()?;
            tx.commit().map_err(|_| StoreError::Indeterminate)?;
            return Ok(CommitOutcome::Unchanged(LoadedTeam {
                instance: self.instance.clone(),
                checkpoint: base,
                digest,
            }));
        }
        let digest = checkpoint::write(&tx, &pending.checkpoint, &self.identity, false)?;
        self.protection.check()?;
        tx.commit().map_err(|_| StoreError::Indeterminate)?;
        self.sync_committed()?;
        #[cfg(test)]
        if self.crash_after_commit {
            std::process::exit(0);
        }
        drop(guard);
        if matches!(pending.checkpoint.state, StoredState::Removed { .. }) {
            let receipt = RemovalReceipt {
                instance: self.instance.clone(),
                revision: pending.checkpoint.revision,
                digest,
            };
            let scrub = self
                .retry_scrub(&receipt)
                .unwrap_or_else(|error| match error {
                    StoreError::Busy => ScrubStatus::PendingBusy,
                    StoreError::UnsafeStorage | StoreError::Binding | StoreError::Conflict => {
                        ScrubStatus::Failed(ScrubFailure::Protection)
                    }
                    _ => ScrubStatus::Failed(ScrubFailure::Storage),
                });
            Ok(CommitOutcome::Removed { receipt, scrub })
        } else {
            Ok(CommitOutcome::Active(LoadedTeam {
                instance: self.instance.clone(),
                checkpoint: pending.checkpoint,
                digest,
            }))
        }
    }
}
