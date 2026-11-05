mod support;

use foundry_budget::{Amount, DeadlineState, ExhaustedCause, Lifecycle, WallClock};
use support::{exhausted, manual_store, money, xts};
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
fn a_deadline_is_exceeded_from_its_exact_millisecond() {
    let (store, clock) = manual_store();
    let budget = store
        .create(xts(), None, WallClock::from_ms(60_000).unwrap())
        .unwrap();
    clock.advance(59_999);
    let snapshot = store.snapshot(budget).unwrap();
    assert_eq!(snapshot.deadline, DeadlineState::Remaining { ms: 1 });
    store
        .reserve(budget, money("0.1"))
        .unwrap()
        .release()
        .unwrap();
    clock.advance(1);
    assert_eq!(
        store.snapshot(budget).unwrap().deadline,
        DeadlineState::Exceeded
    );
    let error = store.reserve(budget, money("0.1")).unwrap_err();
    assert_eq!(exhausted(error), (budget, ExhaustedCause::DeadlineExceeded));
}

#[test]
fn deadline_expiry_suspends_and_refuses_new_dispatch() {
    let (store, clock) = manual_store();
    let budget = store
        .create(xts(), None, WallClock::from_ms(60_000).unwrap())
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
        .create(xts(), None, WallClock::from_ms(60_000).unwrap())
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
    let budget = store.create(xts(), None, WallClock::NONE).unwrap();
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
