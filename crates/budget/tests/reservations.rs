mod support;

use foundry_budget::{
    Amount, BudgetError, BudgetId, BudgetStore, CostUnit, DeadlineState, ExhaustedCause, Lifecycle,
    Reservation, WallClock,
};
use support::{manual_store, money, tokens, xts};

/// A money budget with `committed` already spent and `held` still reserved.
fn money_budget(
    store: &BudgetStore,
    ceiling: &str,
    committed: &str,
    held: &str,
) -> (BudgetId, Reservation) {
    let budget = store
        .open(xts(), Some(money(ceiling)), WallClock::NONE)
        .unwrap();
    store
        .reserve(budget, money(committed))
        .unwrap()
        .commit(money(committed))
        .unwrap();
    let hold = store.reserve(budget, money(held)).unwrap();
    (budget, hold)
}

fn exhausted(error: BudgetError) -> (BudgetId, ExhaustedCause) {
    match error {
        BudgetError::BudgetExhausted { budget, cause } => (budget, cause),
        other => panic!("expected budget_exhausted, got {other}"),
    }
}

#[test]
fn reservation_within_ceiling_succeeds() {
    let (store, _) = manual_store();
    let (budget, _hold) = money_budget(&store, "1.00", "0.40", "0.10");
    let reservation = store.reserve(budget, money("0.50")).unwrap();
    assert_eq!(reservation.amount(), money("0.5"));
    let snapshot = store.snapshot(budget).unwrap();
    assert_eq!(snapshot.reserved, money("0.6"));
    assert_eq!(snapshot.remaining, Some(money("0")));
}

#[test]
fn reservation_over_ceiling_is_budget_exhausted() {
    let (store, _) = manual_store();
    let (budget, _hold) = money_budget(&store, "1.00", "0.40", "0.10");
    let error = store.reserve(budget, money("0.500000001")).unwrap_err();
    assert_eq!(error.class(), "budget_exhausted");
    assert_eq!(exhausted(error), (budget, ExhaustedCause::Ceiling));
    assert_eq!(store.snapshot(budget).unwrap().reserved, money("0.1"));
}

#[test]
fn decimal_reservation_fits_exactly() {
    let (store, _) = manual_store();
    let budget = store
        .open(xts(), Some(money("0.3")), WallClock::NONE)
        .unwrap();
    store
        .reserve(budget, money("0.1"))
        .unwrap()
        .commit(money("0.1"))
        .unwrap();
    let mut exact = store.reserve(budget, money("0.2")).unwrap();
    exact.commit(money("0.2")).unwrap();
    assert_eq!(store.snapshot(budget).unwrap().committed, money("0.3"));
}

#[test]
fn token_ceiling_uses_integer_units() {
    let (store, _) = manual_store();
    let budget = store
        .open(
            CostUnit::OutputTokens,
            Some(tokens(10_000)),
            WallClock::NONE,
        )
        .unwrap();
    store
        .reserve(budget, tokens(9_000))
        .unwrap()
        .commit(tokens(9_000))
        .unwrap();
    let _held = store.reserve(budget, tokens(500)).unwrap();
    let _winner = store.reserve(budget, tokens(500)).unwrap();
    assert_eq!(
        store.reserve(budget, tokens(500)).unwrap_err().class(),
        "budget_exhausted"
    );
}

#[test]
fn reservations_in_another_unit_are_invalid() {
    let (store, _) = manual_store();
    let budget = store
        .open(xts(), Some(money("1")), WallClock::NONE)
        .unwrap();
    assert_eq!(
        store.reserve(budget, tokens(1)).unwrap_err().class(),
        "invalid_request"
    );
    let mismatched = store.open(xts(), Some(tokens(1)), WallClock::NONE);
    assert_eq!(mismatched.unwrap_err().class(), "invalid_request");
}

#[test]
fn settle_below_reservation_releases_rest() {
    let (store, _) = manual_store();
    let budget = store
        .open(xts(), Some(money("1.00")), WallClock::NONE)
        .unwrap();
    store
        .reserve(budget, money("0.40"))
        .unwrap()
        .commit(money("0.40"))
        .unwrap();
    let settlement = store
        .reserve(budget, money("0.50"))
        .unwrap()
        .commit(money("0.20"))
        .unwrap();
    assert_eq!(settlement.committed, money("0.60"));
    assert_eq!(settlement.released, money("0.30"));
    assert!(!settlement.overrun);
    assert!(settlement.dispatch_allowed);
    assert_eq!(store.snapshot(budget).unwrap().reserved, money("0"));
}

#[test]
fn settle_above_reservation_records_overrun_and_refuses_dispatch() {
    let (store, _) = manual_store();
    let budget = store
        .open(xts(), Some(money("1.00")), WallClock::NONE)
        .unwrap();
    store
        .reserve(budget, money("0.40"))
        .unwrap()
        .commit(money("0.40"))
        .unwrap();
    let settlement = store
        .reserve(budget, money("0.50"))
        .unwrap()
        .commit(money("0.55"))
        .unwrap();
    assert_eq!(settlement.committed, money("0.95"));
    assert_eq!(settlement.released, money("0"));
    assert!(settlement.overrun);
    assert!(!settlement.dispatch_allowed);
    let error = store.reserve(budget, money("0.01")).unwrap_err();
    assert_eq!(exhausted(error), (budget, ExhaustedCause::Overrun));
}

#[test]
fn cancelled_call_settles_consumed_amount() {
    let (store, _) = manual_store();
    let budget = store
        .open(xts(), Some(money("1.00")), WallClock::NONE)
        .unwrap();
    let settlement = store
        .reserve(budget, money("0.50"))
        .unwrap()
        .commit(money("0.05"))
        .unwrap();
    assert_eq!(settlement.committed, money("0.05"));
    assert_eq!(settlement.released, money("0.45"));
    assert!(settlement.dispatch_allowed);
}

#[test]
fn dropping_an_unsettled_reservation_releases_it() {
    let (store, _) = manual_store();
    let budget = store
        .open(xts(), Some(money("1")), WallClock::NONE)
        .unwrap();
    let reservation = store.reserve(budget, money("1")).unwrap();
    assert!(store.reserve(budget, money("0.1")).is_err());
    drop(reservation);
    let snapshot = store.snapshot(budget).unwrap();
    assert_eq!(
        (snapshot.committed, snapshot.reserved),
        (money("0"), money("0"))
    );
    let mut released = store.reserve(budget, money("1")).unwrap();
    assert_eq!(released.release().unwrap().released, money("1"));
}

#[test]
fn a_reservation_settles_once_and_survives_a_bad_commit() {
    let (store, _) = manual_store();
    let budget = store
        .open(xts(), Some(money("1")), WallClock::NONE)
        .unwrap();
    let mut reservation = store.reserve(budget, money("0.5")).unwrap();
    assert_eq!(
        reservation.commit(tokens(1)).unwrap_err().class(),
        "invalid_request"
    );
    assert_eq!(store.snapshot(budget).unwrap().reserved, money("0.5"));
    reservation.commit(money("0.2")).unwrap();
    assert_eq!(reservation.release().unwrap_err().class(), "conflict");
    drop(reservation);
    assert_eq!(store.snapshot(budget).unwrap().committed, money("0.2"));
}

#[test]
fn child_budget_cannot_widen_parent() {
    let (store, _) = manual_store();
    let parent = store
        .open(xts(), Some(money("1.00")), WallClock::NONE)
        .unwrap();
    let error = store
        .delegate(parent, Some(money("2.00")), WallClock::NONE)
        .unwrap_err();
    assert_eq!(
        error,
        BudgetError::InvalidRequest {
            field: "child_ceiling"
        }
    );
    let inherited = store.delegate(parent, None, WallClock::NONE).unwrap();
    assert_eq!(
        store.snapshot(inherited).unwrap().remaining,
        Some(money("1.00"))
    );
}

#[test]
fn child_ceiling_is_capped_by_parent_remainder() {
    let (store, _) = manual_store();
    let parent = store
        .open(xts(), Some(money("1.00")), WallClock::NONE)
        .unwrap();
    store
        .reserve(parent, money("0.70"))
        .unwrap()
        .commit(money("0.70"))
        .unwrap();
    assert!(
        store
            .delegate(parent, Some(money("0.31")), WallClock::NONE)
            .is_err()
    );
    let child = store
        .delegate(parent, Some(money("0.30")), WallClock::NONE)
        .unwrap();
    let grandchild = store.delegate(child, None, WallClock::NONE).unwrap();
    assert!(
        store
            .delegate(grandchild, Some(money("0.31")), WallClock::NONE)
            .is_err()
    );

    let _held = store.reserve(grandchild, money("0.25")).unwrap();
    assert_eq!(store.snapshot(parent).unwrap().reserved, money("0.25"));
    assert_eq!(
        store.snapshot(child).unwrap().remaining,
        Some(money("0.05"))
    );
    let error = store.reserve(parent, money("0.06")).unwrap_err();
    assert_eq!(exhausted(error), (parent, ExhaustedCause::Ceiling));
}

#[test]
fn a_child_refused_by_an_ancestor_names_the_ancestor() {
    let (store, _) = manual_store();
    let parent = store
        .open(xts(), Some(money("1")), WallClock::NONE)
        .unwrap();
    let child = store
        .delegate(parent, Some(money("1")), WallClock::NONE)
        .unwrap();
    store
        .reserve(parent, money("0.6"))
        .unwrap()
        .commit(money("0.6"))
        .unwrap();
    let error = store.reserve(child, money("0.5")).unwrap_err();
    assert_eq!(exhausted(error), (parent, ExhaustedCause::Ceiling));
}

#[test]
fn child_overrun_pushing_parent_past_ceiling_stops_the_parent() {
    let (store, _) = manual_store();
    let parent = store
        .open(xts(), Some(money("1")), WallClock::NONE)
        .unwrap();
    let child = store.delegate(parent, None, WallClock::NONE).unwrap();
    let settlement = store
        .reserve(child, money("0.9"))
        .unwrap()
        .commit(money("1.2"))
        .unwrap();
    assert!(settlement.overrun);
    assert!(store.snapshot(parent).unwrap().overrun);
    let error = store.reserve(parent, money("0")).unwrap_err();
    assert_eq!(exhausted(error), (parent, ExhaustedCause::Overrun));
}

#[test]
fn unknown_budgets_are_not_found() {
    let (store, _) = manual_store();
    let budget = store.open(xts(), None, WallClock::NONE).unwrap();
    let other = BudgetStore::new(std::sync::Arc::new(foundry_budget::MonotonicClock::new()));
    assert_eq!(
        other.reserve(budget, money("1")).unwrap_err().class(),
        "not_found"
    );
    assert_eq!(
        other
            .delegate(budget, None, WallClock::NONE)
            .unwrap_err()
            .class(),
        "not_found"
    );
}

#[test]
fn deadline_limits_follow_the_specification() {
    assert_eq!(WallClock::from_ms(0).unwrap(), WallClock::NONE);
    assert_eq!(WallClock::from_ms(0).unwrap().ms(), None);
    assert_eq!(WallClock::from_ms(60_000).unwrap().ms(), Some(60_000));
    assert_eq!(
        WallClock::from_ms(3_600_000_000).unwrap().ms(),
        Some(3_600_000_000)
    );
    for ms in [1, 59_999, 3_600_000_001] {
        assert_eq!(
            WallClock::from_ms(ms).unwrap_err().class(),
            "invalid_request",
            "{ms}"
        );
    }
}

#[test]
fn deadline_expiry_suspends_and_refuses_new_dispatch() {
    let (store, clock) = manual_store();
    let budget = store
        .open(xts(), None, WallClock::from_ms(60_000).unwrap())
        .unwrap();
    let mut running = store.reserve(budget, money("0.1")).unwrap();
    // A recovery wait is ordinary elapsed time and counts against the deadline.
    clock.advance(60_001);
    let deadline = store.snapshot(budget).unwrap().deadline;
    assert_eq!(deadline, DeadlineState::Exceeded);
    let termination = deadline.termination().unwrap();
    assert_eq!(termination.lifecycle, Lifecycle::Suspended);
    assert_eq!(termination.outcome, None);
    assert_eq!(termination.cause.name(), "deadline_exceeded");
    let error = store.reserve(budget, money("0.01")).unwrap_err();
    assert_eq!(exhausted(error), (budget, ExhaustedCause::DeadlineExceeded));
    let settlement = running.commit(money("0.05")).unwrap();
    assert!(!settlement.dispatch_allowed);
    assert_eq!(settlement.committed, money("0.05"));
}

#[test]
fn child_deadline_never_outlasts_the_parent() {
    let (store, clock) = manual_store();
    let parent = store
        .open(xts(), None, WallClock::from_ms(60_000).unwrap())
        .unwrap();
    clock.advance(30_000);
    let child = store
        .delegate(parent, None, WallClock::from_ms(600_000).unwrap())
        .unwrap();
    let untimed = store.delegate(parent, None, WallClock::NONE).unwrap();
    for budget in [child, untimed] {
        let remaining = store.snapshot(budget).unwrap().deadline;
        assert_eq!(remaining, DeadlineState::Remaining { ms: 30_000 });
    }
    clock.advance(30_000);
    let error = store.reserve(child, Amount::zero(xts())).unwrap_err();
    assert_eq!(exhausted(error), (parent, ExhaustedCause::DeadlineExceeded));
}

#[test]
fn no_deadline_never_expires() {
    let (store, clock) = manual_store();
    let budget = store.open(xts(), None, WallClock::NONE).unwrap();
    clock.advance(u64::MAX / 2);
    assert_eq!(
        store.snapshot(budget).unwrap().deadline,
        DeadlineState::Unbounded
    );
    store
        .reserve(budget, money("5"))
        .unwrap()
        .commit(money("5"))
        .unwrap();
}
