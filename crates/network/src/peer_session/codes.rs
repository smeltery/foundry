use super::*;
use crate::{
    code_invite::DerivedCode,
    full_team_store::{CodeLease, ControlEffect, IssuedCodeInvite, StoreError},
};

/// Durable issuance plus its publication permit. Keep through bounded local
/// reply output; do not hold while waiting for a new code listener to join.
pub struct IssuedInvitation {
    _guard: OwnedMutexGuard<Gate>,
    issued: IssuedCodeInvite,
}
impl IssuedInvitation {
    pub fn issued(&self) -> &IssuedCodeInvite {
        &self.issued
    }
}

/// Revalidated generation and privileged derived material. Keep alive across one
/// bounded bind/publication effect. Drop before listening for JOIN; obtain a
/// FRESH permit before publishing a completed bind. It is not a listener lifetime
/// lease, a consumed-reply lease, or evidence that an external effect occurred.
pub struct CodePermit {
    _guard: OwnedMutexGuard<Gate>,
    lease: Arc<CodeLease>,
    derived: DerivedCode,
}
impl CodePermit {
    /// Protocol configuration bound by the actual protected store policy.
    pub fn profile(&self) -> &WireProfile {
        self._guard.store.wire_profile()
    }

    pub fn lease(&self) -> &CodeLease {
        &self.lease
    }
    pub fn derived(&self) -> &DerivedCode {
        &self.derived
    }
}
impl AdmittedTeam {
    pub async fn issue_code(&self, hours: u64, own: bool) -> Result<IssuedInvitation> {
        let mut guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            guard.current()?;
            let now = guard.now()?;
            let base = guard.store.load(now).map_err(store_error)?;
            let pending = guard
                .store
                .prepare_code_intent(&base, hours, own, now)
                .map_err(store_error)?;
            let now = guard.now()?;
            let issued = guard
                .store
                .commit_code_intent(pending, now)
                .map_err(|error| {
                    guard.quiescent = true;
                    store_error(error)
                })?;
            Ok(IssuedInvitation {
                _guard: guard,
                issued,
            })
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
    pub async fn recover_code_intents(&self) -> Result<Vec<Arc<CodeLease>>> {
        let mut guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            guard.current()?;
            let now = guard.now()?;
            guard
                .store
                .recover_code_intents(now)
                .map(|leases| leases.into_iter().map(Arc::new).collect())
                .map_err(store_error)
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
    pub async fn code_permit(&self, lease: Arc<CodeLease>) -> Result<CodePermit> {
        let mut guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            guard.current()?;
            let now = guard.now()?;
            let derived = guard
                .store
                .validate_code_lease(&lease, now)
                .map_err(store_error)?;
            Ok(CodePermit {
                _guard: guard,
                lease,
                derived,
            })
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
    pub async fn code_effects(&self) -> Result<Vec<ControlEffect>> {
        let mut guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            guard.current()?;
            let now = guard.now()?;
            guard.store.code_effects(now).map_err(store_error)
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
    /// Acknowledge only an exact descriptor AFTER the host performed its effect.
    /// A stale descriptor cannot delete a newer generation's desired work.
    pub async fn acknowledge_code_effect(&self, effect: ControlEffect) -> Result<()> {
        let mut guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            guard.current()?;
            let now = guard.now()?;
            let base = guard.store.load(now).map_err(store_error)?;
            let pending = guard
                .store
                .prepare_effect_ack(&base, &effect, now)
                .map_err(store_error)?;
            guard
                .store
                .commit_transition(pending, now)
                .map_err(|error| {
                    guard.quiescent = true;
                    store_error(error)
                })?;
            Ok(())
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
}
pub(super) fn store_error(error: StoreError) -> PeerError {
    match error {
        StoreError::Unsupported => PeerError::Unsupported,
        StoreError::Invalid | StoreError::Corrupt => PeerError::Invalid,
        StoreError::Binding => PeerError::Unauthorized,
        StoreError::Conflict => PeerError::StaleEpoch,
        StoreError::TooLarge => PeerError::Limit,
        _ => PeerError::Storage,
    }
}
