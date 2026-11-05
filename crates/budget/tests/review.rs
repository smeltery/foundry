use foundry_budget::{
    Cause, Lifecycle, Outcome, QuotaCause, ReviewPolicy, ReviewStep, Termination,
};

#[test]
fn review_rounds_default_to_four_including_first_pass() {
    let policy = ReviewPolicy::new(true, None, false).unwrap();
    assert_eq!(policy.rounds_max(), 4);
    let mut rounds = policy.start();
    assert_eq!(
        rounds.record(false).unwrap(),
        ReviewStep::Continue { next_round: 2 }
    );
    assert_eq!(
        rounds.record(false).unwrap(),
        ReviewStep::Continue { next_round: 3 }
    );
    assert_eq!(
        rounds.record(false).unwrap(),
        ReviewStep::Continue { next_round: 4 }
    );
    assert_eq!(
        rounds.record(false).unwrap(),
        ReviewStep::Ended(Termination::needs_review())
    );
    assert_eq!(rounds.completed(), 4);
}

#[test]
fn unverifiable_review_end_needs_review() {
    let termination = Termination::needs_review();
    assert_eq!(termination.lifecycle, Lifecycle::Exit);
    assert_eq!(termination.outcome, Some(Outcome::Failure));
    assert_eq!(termination.cause, Cause::NeedsReview);
    assert_eq!(
        (termination.lifecycle.name(), termination.cause.name()),
        ("exit", "needs_review")
    );
    assert_eq!(Outcome::Failure.name(), "failure");
}

#[test]
fn verified_round_ends_review_without_deciding_success() {
    let mut rounds = ReviewPolicy::new(true, Some(3), false).unwrap().start();
    assert!(matches!(
        rounds.record(false).unwrap(),
        ReviewStep::Continue { .. }
    ));
    assert_eq!(rounds.record(true).unwrap(), ReviewStep::Verified);
    assert_eq!(rounds.record(false).unwrap_err().class(), "conflict");
}

#[test]
fn review_round_limits_follow_the_specification() {
    assert_eq!(
        ReviewPolicy::new(true, Some(1), false)
            .unwrap()
            .rounds_max(),
        1
    );
    assert_eq!(
        ReviewPolicy::new(true, Some(16), false)
            .unwrap()
            .rounds_max(),
        16
    );
    for rounds in [0, 17, u32::MAX] {
        let error = ReviewPolicy::new(true, Some(rounds), false).unwrap_err();
        assert_eq!(error.class(), "invalid_request", "{rounds}");
    }
}

#[test]
fn follow_ups_and_short_work_run_a_single_pass() {
    assert_eq!(
        ReviewPolicy::new(true, Some(8), true).unwrap().rounds_max(),
        1
    );
    assert_eq!(
        ReviewPolicy::new(false, None, false).unwrap().rounds_max(),
        1
    );
    let mut single = ReviewPolicy::new(true, None, true).unwrap().start();
    assert_eq!(
        single.record(false).unwrap(),
        ReviewStep::Ended(Termination::needs_review())
    );
}

#[test]
fn quota_exhaustion_suspends_with_a_cause_code() {
    for (cause, code) in [
        (QuotaCause::AccountLimit, "account_limit"),
        (QuotaCause::PlanLimit, "plan_limit"),
        (QuotaCause::Billing, "billing"),
    ] {
        let termination = Termination::quota_exhausted(cause);
        assert_eq!(termination.lifecycle, Lifecycle::Suspended);
        assert_eq!(termination.lifecycle.name(), "suspended");
        assert_eq!(termination.outcome, None);
        assert_eq!(termination.cause, Cause::QuotaExhausted(cause));
        assert_eq!(termination.cause.name(), "quota_exhausted");
        assert_eq!(cause.name(), code);
    }
}

#[test]
fn deadline_termination_is_suspended_without_an_outcome() {
    let termination = Termination::deadline_exceeded();
    assert_eq!(termination.lifecycle, Lifecycle::Suspended);
    assert_eq!(termination.outcome, None);
    assert_eq!(termination.cause.name(), "deadline_exceeded");
}
