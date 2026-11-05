#![allow(dead_code)]
//! Neutral fixture accounts and helpers shared by the rollover test files,
//! included with `#[path]`. No network calls and no credentials.

pub use foundry_identity::authz::WorkspaceId;
pub use foundry_providers::catalog::{ModelId, ModelRef, ProviderId};
pub use foundry_providers::credentials::{AccountKey, CredentialState};
pub use foundry_providers::rollover::{
    AccountCandidate, AttemptFailure, AttemptId, AttemptState, HoldCause, RolloverDecision,
    RolloverError, RolloverPolicy, RolloverRecord, RolloverRequest, RolloverTrigger, SkipCause,
    SkippedAccount, decide,
};

pub const WORKSPACE: &str = "workspace-a";
pub const PROVIDER: &str = "hosted-a";
pub const OPTED_IN: RolloverPolicy = RolloverPolicy {
    allow_rollover: true,
    isolated: false,
};

pub fn account(key: &str) -> AccountKey {
    AccountKey::new(key).unwrap()
}

pub fn attempt(id: &str) -> AttemptId {
    AttemptId::new(id).unwrap()
}

pub fn model() -> ModelRef {
    ModelRef {
        provider_id: ProviderId::new(PROVIDER).unwrap(),
        model_id: ModelId::new("hosted-large").unwrap(),
    }
}

pub fn candidate(key: &str) -> AccountCandidate {
    AccountCandidate {
        workspace: WorkspaceId::new(WORKSPACE),
        provider_id: ProviderId::new(PROVIDER).unwrap(),
        account_key: account(key),
        state: CredentialState::Configured,
        available: true,
    }
}

pub fn ended(failure: AttemptFailure) -> AttemptState {
    AttemptState::Ended {
        attempt_id: attempt("attempt-1"),
        failure,
    }
}

pub fn request<'a>(
    policy: RolloverPolicy,
    attempt_state: AttemptState,
    candidates: &'a [AccountCandidate],
) -> RolloverRequest<'a> {
    RolloverRequest {
        policy,
        workspace: WorkspaceId::new(WORKSPACE),
        model: model(),
        account_key: account("account-1"),
        attempt: attempt_state,
        next_attempt_id: attempt("attempt-2"),
        history: &[],
        candidates,
    }
}

/// A quota rollover record of the fixture workspace and model.
pub fn link(from: &str, from_attempt: &str, to: &str, to_attempt: &str) -> RolloverRecord {
    RolloverRecord {
        workspace: WorkspaceId::new(WORKSPACE),
        policy: OPTED_IN,
        model: model(),
        from_account: account(from),
        to_account: account(to),
        trigger: RolloverTrigger::QuotaExhausted,
        from_attempt: attempt(from_attempt),
        to_attempt: attempt(to_attempt),
        skipped: Vec::new(),
    }
}

pub fn rolled(decision: Result<RolloverDecision, RolloverError>) -> RolloverRecord {
    match decision {
        Ok(RolloverDecision::RollOver(record)) => *record,
        other => panic!("expected a rollover, got {other:?}"),
    }
}
