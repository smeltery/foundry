mod support;

use foundry_budget::{
    BudgetId, BudgetStore, CostUnit, ExhaustedCause, Reservation, SystemClock, WallClock,
};
use support::{exhausted, manual_store, money, tokens, xts};
/// A money budget with `committed` already spent and `held` still reserved.
fn money_budget(
    store: &BudgetStore,
    ceiling: &str,
    committed: &str,
    held: &str,
) -> (BudgetId, Reservation) {
    let budget = store
        .create(xts(), Some(money(ceiling)), WallClock::NONE)
        .unwrap();
    store
        .reserve(budget, money(committed))
        .unwrap()
        .commit(money(committed))
        .unwrap();
    let hold = store.reserve(budget, money(held)).unwrap();
    (budget, hold)
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
        .create(xts(), Some(money("0.3")), WallClock::NONE)
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
        .create(
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
        .create(xts(), Some(money("1")), WallClock::NONE)
        .unwrap();
    assert_eq!(
        store.reserve(budget, tokens(1)).unwrap_err().class(),
        "invalid_request"
    );
    let mismatched = store.create(xts(), Some(tokens(1)), WallClock::NONE);
    assert_eq!(mismatched.unwrap_err().class(), "invalid_request");
}

#[test]
fn settle_below_reservation_releases_rest() {
    let (store, _) = manual_store();
    let budget = store
        .create(xts(), Some(money("1.00")), WallClock::NONE)
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
        .create(xts(), Some(money("1.00")), WallClock::NONE)
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
        .create(xts(), Some(money("1.00")), WallClock::NONE)
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
fn dropping_an_unsettled_reservation_charges_it_in_full() {
    let (store, _) = manual_store();
    let budget = store
        .create(xts(), Some(money("1")), WallClock::NONE)
        .unwrap();
    let reservation = store.reserve(budget, money("0.4")).unwrap();
    drop(reservation);
    let snapshot = store.snapshot(budget).unwrap();
    assert_eq!(
        (snapshot.committed, snapshot.reserved),
        (money("0.4"), money("0"))
    );
    assert!(!snapshot.overrun);
    let mut released = store.reserve(budget, money("0.6")).unwrap();
    assert_eq!(released.release().unwrap().released, money("0.6"));
    drop(released);
    assert_eq!(store.snapshot(budget).unwrap().committed, money("0.4"));
}

#[test]
fn a_reservation_settles_once_and_survives_a_bad_commit() {
    let (store, _) = manual_store();
    let budget = store
        .create(xts(), Some(money("1")), WallClock::NONE)
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
fn unknown_budgets_are_not_found() {
    let (store, _) = manual_store();
    let budget = store.create(xts(), None, WallClock::NONE).unwrap();
    let other = BudgetStore::open_in_memory(std::sync::Arc::new(SystemClock)).unwrap();
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
