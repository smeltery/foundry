#[path = "rollover_support.rs"]
mod support;

use support::*;

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
                history.push(*record);
            }
            RolloverDecision::Hold(cause) => {
                held = Some(cause);
                break;
            }
        }
    }
    // Every account has had a turn, so the cap ends the chain.
    assert_eq!(held, Some(HoldCause::ChainLimit));
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

    let earlier = link("account-0", "attempt-0", "account-1", "attempt-1");
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

#[test]
fn broken_or_foreign_chains_are_refused() {
    let candidates = [candidate("account-1"), candidate("account-4")];
    let quota = || ended(AttemptFailure::QuotaExhausted);
    let refused = |history: &[RolloverRecord]| {
        let mut chained = request(OPTED_IN, quota(), &candidates);
        chained.history = history;
        decide(&chained)
    };

    // The latest record must lead to the current attempt, not only the account.
    let stale = [link("account-0", "attempt-0", "account-1", "attempt-7")];
    assert_eq!(refused(&stale), Err(RolloverError::HistoryMismatch));

    let mut other_model = link("account-0", "attempt-0", "account-1", "attempt-1");
    other_model.model.model_id = ModelId::new("hosted-small").unwrap();
    assert_eq!(refused(&[other_model]), Err(RolloverError::HistoryMismatch));

    let mut other_workspace = link("account-0", "attempt-0", "account-1", "attempt-1");
    other_workspace.workspace = WorkspaceId::new("workspace-b");
    assert_eq!(
        refused(&[other_workspace]),
        Err(RolloverError::HistoryMismatch)
    );

    let account_gap = [
        link("account-2", "attempt-0", "account-3", "attempt-9"),
        link("account-0", "attempt-9", "account-1", "attempt-1"),
    ];
    assert_eq!(refused(&account_gap), Err(RolloverError::BrokenChain));

    let attempt_gap = [
        link("account-2", "attempt-0", "account-0", "attempt-8"),
        link("account-0", "attempt-9", "account-1", "attempt-1"),
    ];
    assert_eq!(refused(&attempt_gap), Err(RolloverError::BrokenChain));

    let repeated_attempt = [
        link("account-2", "attempt-1", "account-0", "attempt-1"),
        link("account-0", "attempt-1", "account-1", "attempt-1"),
    ];
    assert_eq!(refused(&repeated_attempt), Err(RolloverError::BrokenChain));

    let linked = [
        link("account-2", "attempt-0", "account-0", "attempt-9"),
        link("account-0", "attempt-9", "account-1", "attempt-1"),
    ];
    let mut wider = request(OPTED_IN, quota(), &candidates);
    wider.history = &linked;
    let more = [
        candidate("account-1"),
        candidate("account-4"),
        candidate("account-5"),
        candidate("account-6"),
    ];
    wider.candidates = &more;
    assert_eq!(rolled(decide(&wider)).to_account, account("account-4"));
    assert_eq!(RolloverError::BrokenChain.class(), "invalid_request");
}

#[test]
fn chain_limit_caps_rollovers_at_one_fewer_than_the_accounts() {
    // The chain ran on accounts no longer offered, so account-2 is untried,
    // but two rollovers already match the two usable accounts minus one.
    let candidates = [candidate("account-1"), candidate("account-2")];
    let history = [
        link("account-8", "attempt-8", "account-9", "attempt-9"),
        link("account-9", "attempt-9", "account-1", "attempt-1"),
    ];
    let mut capped = request(OPTED_IN, ended(AttemptFailure::QuotaExhausted), &candidates);
    capped.history = &history;
    assert_eq!(
        decide(&capped),
        Ok(RolloverDecision::Hold(HoldCause::ChainLimit))
    );
    assert_eq!(HoldCause::ChainLimit.name(), "chain_limit");

    // A new turn or model change starts a fresh chain with no records.
    capped.history = &[];
    assert_eq!(rolled(decide(&capped)).to_account, account("account-2"));
}
