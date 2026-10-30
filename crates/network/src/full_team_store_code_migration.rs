//! Explicit policy-enabling migration. v2 bytes and signatures remain supported
//! under a code-unavailable policy; ordinary open never rewrites either format.
use super::*;
impl FullTeamStore {
    pub fn enable_codes(mut self, codes: crate::code_invite::CodePolicy, now: i64) -> Result<Self> {
        if now < 0 || self.policy.codes.is_some() {
            return Err(StoreError::Invalid);
        }
        let policy = StorePolicy {
            profile: self.policy.profile.clone(),
            verifier: self.policy.verifier.clone(),
            digest: self.policy.digest,
            codes: None,
        }
        .with_codes(codes)?;
        let guard = self.lock.lock(self.timeout)?;
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut checkpoint, old_digest) = checkpoint::read(&tx, &self.identity, &self.policy)?;
        if checkpoint.incarnation != self.instance.incarnation {
            return Err(StoreError::Binding);
        }
        validation::validate(&checkpoint, &self.policy, now)?;
        checkpoint.configure(&policy);
        checkpoint.policy = policy.digest;
        if checkpoint.revision > 0 {
            checkpoint.revision = checkpoint
                .revision
                .checked_add(1)
                .ok_or(StoreError::RevisionOverflow)?;
            checkpoint.parent = old_digest;
            checkpoint.network_generation = 1;
            checkpoint.accepted_ms = checkpoint.accepted_ms.max(now);
            checkpoint.operation_ms = now;
        }
        validation::validate(&checkpoint, &policy, now)?;
        if checkpoint.revision > i64::MAX as u64 {
            return Err(StoreError::RevisionOverflow);
        }
        checkpoint::write(&tx, &checkpoint, &self.identity, false)?;
        tx.pragma_update(None, "user_version", 3)?;
        self.protection.check()?;
        tx.commit().map_err(|_| StoreError::Indeterminate)?;
        self.sync_committed()?;
        drop(guard);
        self.policy = policy;
        self.instance = Arc::new(StoreInstance {
            incarnation: checkpoint.incarnation,
        });
        Ok(self)
    }
}
