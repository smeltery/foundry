use super::{
    AccountCandidate, AttemptFailure, AttemptId, AttemptState, RolloverError, SkippedAccount,
};
use crate::catalog::ModelRef;
use crate::credentials::AccountKey;
use foundry_identity::authz::WorkspaceId;
use std::collections::{BTreeMap, BTreeSet};

/// Whether a session may roll over. The default allows nothing; only an
/// explicit request field turns rollover on, and an isolated benchmark session
/// never rolls over even then.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RolloverPolicy {
    pub allow_rollover: bool,
    pub isolated: bool,
}

/// Everything one decision reads. The caller supplies the ID the new attempt
/// will take, so the decision stays pure and its record can link to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloverRequest<'a> {
    pub policy: RolloverPolicy,
    pub workspace: WorkspaceId,
    /// The session's model; its provider bounds the accounts considered.
    pub model: ModelRef,
    /// The account the current attempt runs or ran on.
    pub account_key: AccountKey,
    pub attempt: AttemptState,
    pub next_attempt_id: AttemptId,
    /// The session's earlier rollovers, oldest first.
    pub history: &'a [RolloverRecord],
    pub candidates: &'a [AccountCandidate],
}

/// The account limit that caused a rollover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolloverTrigger {
    QuotaExhausted,
    RateLimited { retry_after_secs: u64 },
}

impl RolloverTrigger {
    pub fn name(self) -> &'static str {
        match self {
            Self::QuotaExhausted => "quota_exhausted",
            Self::RateLimited { .. } => "rate_limited",
        }
    }

    fn from_failure(failure: AttemptFailure) -> Option<Self> {
        match failure {
            AttemptFailure::QuotaExhausted => Some(Self::QuotaExhausted),
            AttemptFailure::RateLimited { retry_after_secs } => {
                Some(Self::RateLimited { retry_after_secs })
            }
            _ => None,
        }
    }
}

/// The record of one rollover, stored with the session. The model never
/// changes; only the account does, and only between attempts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloverRecord {
    pub model: ModelRef,
    pub from_account: AccountKey,
    pub to_account: AccountKey,
    pub trigger: RolloverTrigger,
    pub from_attempt: AttemptId,
    pub to_attempt: AttemptId,
    /// Accounts of the same workspace and provider passed over before the
    /// target, in account-key order. Accounts of other workspaces never appear.
    pub skipped: Vec<SkippedAccount>,
}

/// Why the session stays on its account. Without a rollover an ended attempt
/// keeps its own failure; nothing is retried silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldCause {
    AttemptRunning,
    /// The attempt failed for something other than an account limit.
    NotAccountLimit,
    Isolated,
    Disabled,
    /// Every other account was tried, needs attention or is unavailable.
    NoEligibleAccount,
}

impl HoldCause {
    pub fn name(self) -> &'static str {
        match self {
            Self::AttemptRunning => "attempt_running",
            Self::NotAccountLimit => "not_account_limit",
            Self::Isolated => "isolated",
            Self::Disabled => "rollover_disabled",
            Self::NoEligibleAccount => "no_eligible_account",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RolloverDecision {
    /// Start `record.to_attempt` with the same model on `record.to_account`.
    RollOver(RolloverRecord),
    Hold(HoldCause),
}

/// Decides one rollover. Candidates are taken in account-key order, and an
/// account that any attempt of this chain ran on is never chosen again, so a
/// chain ends after at most one rollover per account and the same inputs always
/// give the same decision.
pub fn decide(request: &RolloverRequest<'_>) -> Result<RolloverDecision, RolloverError> {
    let candidates = in_scope(request)?;
    let tried = validate_history(request)?;
    let hold = |cause| Ok(RolloverDecision::Hold(cause));
    let failure = match &request.attempt {
        AttemptState::Running { .. } => return hold(HoldCause::AttemptRunning),
        AttemptState::Ended { failure, .. } => *failure,
    };
    let Some(trigger) = RolloverTrigger::from_failure(failure) else {
        return hold(HoldCause::NotAccountLimit);
    };
    if request.policy.isolated {
        return hold(HoldCause::Isolated);
    }
    if !request.policy.allow_rollover {
        return hold(HoldCause::Disabled);
    }
    let mut skipped = Vec::new();
    for (account_key, candidate) in candidates {
        if *account_key == request.account_key {
            continue;
        }
        match candidate.skip_cause(tried.contains(account_key)) {
            Some(cause) => skipped.push(SkippedAccount {
                account_key: account_key.clone(),
                cause,
            }),
            None => {
                return Ok(RolloverDecision::RollOver(RolloverRecord {
                    model: request.model.clone(),
                    from_account: request.account_key.clone(),
                    to_account: account_key.clone(),
                    trigger,
                    from_attempt: request.attempt.attempt_id().clone(),
                    to_attempt: request.next_attempt_id.clone(),
                    skipped,
                }));
            }
        }
    }
    hold(HoldCause::NoEligibleAccount)
}

/// The candidates of the session's workspace and provider, by account key.
/// Accounts elsewhere are dropped before anything can record them.
fn in_scope<'a>(
    request: &'a RolloverRequest<'_>,
) -> Result<BTreeMap<&'a AccountKey, &'a AccountCandidate>, RolloverError> {
    let mut by_key = BTreeMap::new();
    for candidate in request.candidates.iter().filter(|candidate| {
        candidate.workspace == request.workspace
            && candidate.provider_id == request.model.provider_id
    }) {
        if by_key.insert(&candidate.account_key, candidate).is_some() {
            return Err(RolloverError::DuplicateCandidate);
        }
    }
    Ok(by_key)
}

/// Checks that the records form this session's chain and that the new attempt
/// is new, then returns every account the chain has run on.
fn validate_history<'a>(
    request: &'a RolloverRequest<'_>,
) -> Result<BTreeSet<&'a AccountKey>, RolloverError> {
    if let Some(latest) = request.history.last()
        && (latest.to_account != request.account_key || latest.model != request.model)
    {
        return Err(RolloverError::HistoryMismatch);
    }
    let mut attempts = BTreeSet::from([request.attempt.attempt_id()]);
    let mut tried = BTreeSet::from([&request.account_key]);
    for record in request.history {
        attempts.extend([&record.from_attempt, &record.to_attempt]);
        tried.extend([&record.from_account, &record.to_account]);
    }
    if attempts.contains(&request.next_attempt_id) {
        return Err(RolloverError::AttemptReused);
    }
    Ok(tried)
}
