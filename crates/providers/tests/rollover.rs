use foundry_identity::authz::WorkspaceId;
use foundry_providers::catalog::{ModelId, ModelRef, ProviderId};
use foundry_providers::credentials::{AccountKey, CredentialState};
use foundry_providers::rollover::{
    AccountCandidate, AttemptFailure, AttemptId, AttemptState, HoldCause, RolloverDecision,
    RolloverError, RolloverPolicy, RolloverRecord, RolloverRequest, RolloverTrigger, SkipCause,
    SkippedAccount, decide,
};

const WORKSPACE: &str = "workspace-a";
const PROVIDER: &str = "hosted-a";
const OPTED_IN: RolloverPolicy = RolloverPolicy {
    allow_rollover: true,
    isolated: false,
};

fn account(key: &str) -> AccountKey {
    AccountKey::new(key).unwrap()
}

fn attempt(id: &str) -> AttemptId {
    AttemptId::new(id).unwrap()
}

fn model() -> ModelRef {
    ModelRef {
        provider_id: ProviderId::new(PROVIDER).unwrap(),
        model_id: ModelId::new("hosted-large").unwrap(),
    }
}

fn candidate(key: &str) -> AccountCandidate {
    AccountCandidate {
        workspace: WorkspaceId::new(WORKSPACE),
        provider_id: ProviderId::new(PROVIDER).unwrap(),
        account_key: account(key),
        state: CredentialState::Configured,
        available: true,
    }
}

fn ended(failure: AttemptFailure) -> AttemptState {
    AttemptState::Ended {
        attempt_id: attempt("attempt-1"),
        failure,
    }
}

fn request<'a>(
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

fn rolled(decision: Result<RolloverDecision, RolloverError>) -> RolloverRecord {
    match decision {
        Ok(RolloverDecision::RollOver(record)) => record,
        other => panic!("expected a rollover, got {other:?}"),
    }
}

#[test]
fn rollover_is_off_by_default() {
    let candidates = [candidate("account-1"), candidate("account-2")];
    for failure in [
        AttemptFailure::QuotaExhausted,
        AttemptFailure::RateLimited {
            retry_after_secs: 30,
        },
    ] {
        let decision = decide(&request(
            RolloverPolicy::default(),
            ended(failure),
            &candidates,
        ));
        assert_eq!(decision, Ok(RolloverDecision::Hold(HoldCause::Disabled)));
    }
    assert_eq!(HoldCause::Disabled.name(), "rollover_disabled");
}

#[test]
fn explicit_rollover_uses_same_model_on_another_account() {
    let candidates = [candidate("account-1"), candidate("account-2")];
    let record = rolled(decide(&request(
        OPTED_IN,
        ended(AttemptFailure::QuotaExhausted),
        &candidates,
    )));
    assert_eq!(
        record,
        RolloverRecord {
            model: model(),
            from_account: account("account-1"),
            to_account: account("account-2"),
            trigger: RolloverTrigger::QuotaExhausted,
            from_attempt: attempt("attempt-1"),
            to_attempt: attempt("attempt-2"),
            skipped: Vec::new(),
        }
    );
    assert_eq!(record.trigger.name(), "quota_exhausted");
}

#[test]
fn rate_limited_rollover_records_the_retry_delay() {
    let candidates = [candidate("account-2")];
    let failure = AttemptFailure::RateLimited {
        retry_after_secs: 45,
    };
    let record = rolled(decide(&request(OPTED_IN, ended(failure), &candidates)));
    assert_eq!(
        record.trigger,
        RolloverTrigger::RateLimited {
            retry_after_secs: 45
        }
    );
    assert_eq!(record.trigger.name(), failure.class());
}

#[test]
fn isolated_benchmark_session_never_rolls_over() {
    let candidates = [candidate("account-2")];
    let policy = RolloverPolicy {
        allow_rollover: true,
        isolated: true,
    };
    let decision = decide(&request(
        policy,
        ended(AttemptFailure::QuotaExhausted),
        &candidates,
    ));
    assert_eq!(decision, Ok(RolloverDecision::Hold(HoldCause::Isolated)));
}

#[test]
fn running_attempt_never_rolls_over() {
    let candidates = [candidate("account-2")];
    let running = AttemptState::Running {
        attempt_id: attempt("attempt-1"),
    };
    let decision = decide(&request(OPTED_IN, running, &candidates));
    assert_eq!(
        decision,
        Ok(RolloverDecision::Hold(HoldCause::AttemptRunning))
    );
}

#[test]
fn non_account_limit_failures_never_roll_over() {
    let candidates = [candidate("account-2")];
    for failure in [
        AttemptFailure::BudgetExhausted,
        AttemptFailure::InvalidRequest,
        AttemptFailure::Unauthenticated,
        AttemptFailure::Forbidden,
        AttemptFailure::NotFound,
        AttemptFailure::ModelUnavailable,
        AttemptFailure::EndpointFailure,
        AttemptFailure::Unavailable,
    ] {
        let decision = decide(&request(OPTED_IN, ended(failure), &candidates));
        assert_eq!(
            decision,
            Ok(RolloverDecision::Hold(HoldCause::NotAccountLimit)),
            "{}",
            failure.class()
        );
    }
}

#[test]
fn ineligible_and_foreign_accounts_are_never_chosen() {
    let mut foreign_workspace = candidate("account-0");
    foreign_workspace.workspace = WorkspaceId::new("workspace-b");
    let mut foreign_provider = candidate("account-00");
    foreign_provider.provider_id = ProviderId::new("hosted-b").unwrap();
    let mut refused = candidate("account-2");
    refused.state = CredentialState::AuthFailed;
    let mut expired = candidate("account-3");
    expired.state = CredentialState::ReconnectRequired;
    let mut blocked = candidate("account-4");
    blocked.available = false;
    let candidates = [
        foreign_workspace,
        foreign_provider,
        refused,
        expired,
        blocked,
        candidate("account-5"),
    ];

    let record = rolled(decide(&request(
        OPTED_IN,
        ended(AttemptFailure::QuotaExhausted),
        &candidates,
    )));
    assert_eq!(record.to_account, account("account-5"));
    let skip = |key, cause| SkippedAccount {
        account_key: account(key),
        cause,
    };
    assert_eq!(
        record.skipped,
        [
            skip("account-2", SkipCause::AuthFailed),
            skip("account-3", SkipCause::ReconnectRequired),
            skip("account-4", SkipCause::Unavailable),
        ]
    );

    // Without the eligible account, nothing elsewhere is a fallback.
    let decision = decide(&request(
        OPTED_IN,
        ended(AttemptFailure::QuotaExhausted),
        &candidates[..5],
    ));
    assert_eq!(
        decision,
        Ok(RolloverDecision::Hold(HoldCause::NoEligibleAccount))
    );
}

#[test]
fn exhausted_accounts_end_the_chain_in_account_key_order() {
    // Offered out of order to show the order comes from the account keys.
    let candidates = [
        candidate("account-3"),
        candidate("account-1"),
        candidate("account-2"),
    ];
    let mut history = Vec::new();
    let mut current = account("account-1");
    let mut current_attempt = attempt("attempt-1");
    let mut route = vec![current.clone()];
    let mut held = None;
    // One step more than the number of accounts: the chain must stop by then.
    for step in 2..=candidates.len() + 1 {
        let decision = decide(&RolloverRequest {
            policy: OPTED_IN,
            workspace: WorkspaceId::new(WORKSPACE),
            model: model(),
            account_key: current.clone(),
            attempt: AttemptState::Ended {
                attempt_id: current_attempt.clone(),
                failure: AttemptFailure::QuotaExhausted,
            },
            next_attempt_id: attempt(&format!("attempt-{step}")),
            history: &history,
            candidates: &candidates,
        })
        .unwrap();
        match decision {
            RolloverDecision::RollOver(record) => {
                assert_eq!(record.from_attempt, current_attempt);
                current = record.to_account.clone();
                current_attempt = record.to_attempt.clone();
                route.push(current.clone());
                history.push(record);
            }
            RolloverDecision::Hold(cause) => {
                held = Some(cause);
                break;
            }
        }
    }
    assert_eq!(held, Some(HoldCause::NoEligibleAccount));
    assert_eq!(
        route,
        [
            account("account-1"),
            account("account-2"),
            account("account-3")
        ]
    );
    assert_eq!(history.len(), 2);
    let last = history.last().unwrap();
    assert_eq!(
        last.skipped,
        [SkippedAccount {
            account_key: account("account-1"),
            cause: SkipCause::AlreadyTried,
        }]
    );
}

#[test]
fn same_inputs_give_the_same_decision() {
    let forward = [
        candidate("account-1"),
        candidate("account-2"),
        candidate("account-3"),
    ];
    let reversed = [
        candidate("account-3"),
        candidate("account-2"),
        candidate("account-1"),
    ];
    let quota = || ended(AttemptFailure::QuotaExhausted);
    let first = decide(&request(OPTED_IN, quota(), &forward));
    assert_eq!(first, decide(&request(OPTED_IN, quota(), &forward)));
    assert_eq!(first, decide(&request(OPTED_IN, quota(), &reversed)));
}

#[test]
fn inconsistent_requests_are_refused() {
    let candidates = [candidate("account-2")];
    let quota = || ended(AttemptFailure::QuotaExhausted);

    let mut reused = request(OPTED_IN, quota(), &candidates);
    reused.next_attempt_id = attempt("attempt-1");
    assert_eq!(decide(&reused), Err(RolloverError::AttemptReused));

    let earlier = RolloverRecord {
        model: model(),
        from_account: account("account-0"),
        to_account: account("account-1"),
        trigger: RolloverTrigger::QuotaExhausted,
        from_attempt: attempt("attempt-0"),
        to_attempt: attempt("attempt-1"),
        skipped: Vec::new(),
    };
    let mut relinked = request(OPTED_IN, quota(), &candidates);
    let history = [earlier.clone()];
    relinked.history = &history;
    relinked.next_attempt_id = attempt("attempt-0");
    assert_eq!(decide(&relinked), Err(RolloverError::AttemptReused));

    let mut mismatched = request(OPTED_IN, quota(), &candidates);
    let foreign_history = [RolloverRecord {
        to_account: account("account-9"),
        ..earlier
    }];
    mismatched.history = &foreign_history;
    assert_eq!(decide(&mismatched), Err(RolloverError::HistoryMismatch));

    let duplicated = [candidate("account-2"), candidate("account-2")];
    assert_eq!(
        decide(&request(OPTED_IN, quota(), &duplicated)),
        Err(RolloverError::DuplicateCandidate)
    );

    assert_eq!(AttemptId::new(""), Err(RolloverError::InvalidAttemptId));
    assert_eq!(
        AttemptId::new("attempt\0"),
        Err(RolloverError::InvalidAttemptId)
    );
    assert!(AttemptId::new(&"a".repeat(AttemptId::MAX_BYTES)).is_ok());
    assert_eq!(
        AttemptId::new(&"a".repeat(AttemptId::MAX_BYTES + 1)),
        Err(RolloverError::InvalidAttemptId)
    );
    assert_eq!(RolloverError::AttemptReused.class(), "invalid_request");
}
