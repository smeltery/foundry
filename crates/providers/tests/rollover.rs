#[path = "rollover_support.rs"]
mod support;

use support::*;

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
            skipped: Vec::new(),
            ..link("account-1", "attempt-1", "account-2", "attempt-2")
        }
    );
    assert_eq!(record.workspace, WorkspaceId::new(WORKSPACE));
    assert_eq!(record.policy, OPTED_IN);
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
fn gates_hold_before_a_mismatched_history_is_read() {
    let candidates = [candidate("account-2")];
    let mismatched = [link("account-0", "attempt-0", "account-9", "attempt-9")];
    let running = AttemptState::Running {
        attempt_id: attempt("attempt-1"),
    };
    let isolated = RolloverPolicy {
        allow_rollover: true,
        isolated: true,
    };
    let cases = [
        (
            RolloverPolicy::default(),
            ended(AttemptFailure::QuotaExhausted),
            HoldCause::Disabled,
        ),
        (OPTED_IN, running, HoldCause::AttemptRunning),
        (
            OPTED_IN,
            ended(AttemptFailure::Unauthenticated),
            HoldCause::NotAccountLimit,
        ),
        (
            isolated,
            ended(AttemptFailure::QuotaExhausted),
            HoldCause::Isolated,
        ),
    ];
    for (policy, attempt_state, cause) in cases {
        let mut gated = request(policy, attempt_state, &candidates);
        gated.history = &mismatched;
        assert_eq!(decide(&gated), Ok(RolloverDecision::Hold(cause)));
    }
}
