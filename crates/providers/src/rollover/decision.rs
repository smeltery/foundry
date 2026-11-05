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
///
/// A caller must persist each returned record before starting its new
/// attempt; a chain rebuilt without it could revisit an account.
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
    /// The current chain only, oldest first: the records whose rollovers led,
    /// attempt by attempt, to the current attempt. An attempt not reached by a
    /// rollover, such as a new turn or a model change, starts a fresh chain
    /// with no records.
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

/// The record of one rollover: one ended attempt on one account, linked to the
/// new attempt that continues the same model on another account of the same
/// workspace and provider. It stands on its own when exported apart from the
/// session, so it names the workspace and the policy that allowed it. It holds
/// no credential and no account of another workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolloverRecord {
    pub workspace: WorkspaceId,
    /// The opt-in in force when the rollover was decided.
    pub policy: RolloverPolicy,
    pub model: ModelRef,
    pub from_account: AccountKey,
    pub to_account: AccountKey,
    pub trigger: RolloverTrigger,
    pub from_attempt: AttemptId,
    pub to_attempt: AttemptId,
    /// Accounts of the same workspace and provider passed over before the
    /// target, in account-key order.
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
    /// The chain already holds one rollover fewer than the accounts it can use.
    ChainLimit,
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
            Self::ChainLimit => "chain_limit",
            Self::NoEligibleAccount => "no_eligible_account",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RolloverDecision {
    /// Start `record.to_attempt` with the same model on `record.to_account`.
    RollOver(Box<RolloverRecord>),
    Hold(HoldCause),
}

/// Decides one rollover. The gates come first, in this order: a running
/// attempt, a failure that is not an account limit, an isolated session and
/// rollover turned off each hold before the chain or candidates are read.
///
/// Candidates are taken in account-key order, and an account that any attempt
/// of the chain ran on is never chosen again. A chain also stops once it holds
/// one rollover fewer than the accounts it can use, so the same inputs always
/// give the same decision and no chain loops.
pub fn decide(request: &RolloverRequest<'_>) -> Result<RolloverDecision, RolloverError> {
    let hold = |cause| Ok(RolloverDecision::Hold(cause));
    let (ended_attempt, failure) = match &request.attempt {
        AttemptState::Running { .. } => return hold(HoldCause::AttemptRunning),
        AttemptState::Ended {
            attempt_id,
            failure,
        } => (attempt_id, *failure),
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

    let candidates = in_scope(request)?;
    let tried = validate_chain(request, ended_attempt)?;
    let usable_accounts =
        candidates.len() + usize::from(!candidates.contains_key(&request.account_key));
    if !request.history.is_empty() && request.history.len() + 1 >= usable_accounts {
        return hold(HoldCause::ChainLimit);
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
                return Ok(RolloverDecision::RollOver(Box::new(RolloverRecord {
                    workspace: request.workspace.clone(),
                    policy: request.policy,
                    model: request.model.clone(),
                    from_account: request.account_key.clone(),
                    to_account: account_key.clone(),
                    trigger,
                    from_attempt: ended_attempt.clone(),
                    to_attempt: request.next_attempt_id.clone(),
                    skipped,
                })));
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

/// Checks that the records form one unbroken chain ending at the current
/// attempt and that the new attempt is new, then returns every account the
/// chain has run on.
fn validate_chain<'a>(
    request: &'a RolloverRequest<'_>,
    current_attempt: &'a AttemptId,
) -> Result<BTreeSet<&'a AccountKey>, RolloverError> {
    let history = request.history;
    if let Some(latest) = history.last()
        && (latest.to_attempt != *current_attempt || latest.to_account != request.account_key)
    {
        return Err(RolloverError::HistoryMismatch);
    }
    if history
        .iter()
        .any(|record| record.workspace != request.workspace || record.model != request.model)
    {
        return Err(RolloverError::HistoryMismatch);
    }
    if history.windows(2).any(|pair| {
        pair[0].to_account != pair[1].from_account || pair[0].to_attempt != pair[1].from_attempt
    }) {
        return Err(RolloverError::BrokenChain);
    }
    // Linked records name each attempt once: every source, then the current one.
    let mut attempts = BTreeSet::from([current_attempt]);
    for record in history {
        if !attempts.insert(&record.from_attempt) {
            return Err(RolloverError::BrokenChain);
        }
    }
    if attempts.contains(&request.next_attempt_id) {
        return Err(RolloverError::AttemptReused);
    }
    let mut tried = BTreeSet::from([&request.account_key]);
    tried.extend(history.iter().map(|record| &record.from_account));
    Ok(tried)
}
