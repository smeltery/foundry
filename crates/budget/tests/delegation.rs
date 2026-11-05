//! Delegated child budgets share their ancestors' spend.
mod support;

use foundry_budget::{BudgetError, ExhaustedCause, WallClock};
use support::{exhausted, manual_store, money, xts};
#[test]
fn child_budget_cannot_widen_parent() {
    let (store, _) = manual_store();
    let parent = store
        .create(xts(), Some(money("1.00")), WallClock::NONE)
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
        .create(xts(), Some(money("1.00")), WallClock::NONE)
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
        .create(xts(), Some(money("1")), WallClock::NONE)
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
        .create(xts(), Some(money("1")), WallClock::NONE)
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
fn an_overrun_blocks_every_budget_sharing_the_chain() {
    let (store, _) = manual_store();
    let root = store
        .create(xts(), Some(money("10")), WallClock::NONE)
        .unwrap();
    let child = store.delegate(root, None, WallClock::NONE).unwrap();
    let grandchild = store.delegate(child, None, WallClock::NONE).unwrap();
    let sibling = store.delegate(root, None, WallClock::NONE).unwrap();
    let mut in_flight = store.reserve(sibling, money("1")).unwrap();
    let settlement = store
        .reserve(child, money("1"))
        .unwrap()
        .commit(money("1.5"))
        .unwrap();
    assert!(settlement.overrun);
    assert!(!settlement.dispatch_allowed);
    // Far below the root ceiling, yet nothing sharing the chain may dispatch.
    for (budget, refuser) in [
        (root, root),
        (child, child),
        (grandchild, child),
        (sibling, root),
    ] {
        let error = store.reserve(budget, money("0.01")).unwrap_err();
        assert_eq!(exhausted(error), (refuser, ExhaustedCause::Overrun));
    }
    // A call already in flight still settles what it consumed.
    let late = in_flight.commit(money("0.5")).unwrap();
    assert!(!late.overrun);
    assert!(!late.dispatch_allowed);
    assert_eq!(store.snapshot(root).unwrap().committed, money("2"));
    // The sibling's own spend did not overrun, but its shared chain did.
    assert!(store.snapshot(sibling).unwrap().overrun);
}
