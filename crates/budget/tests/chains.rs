//! Concurrency across delegation chains: children, grandchildren, delegation
//! racing reservation, and overruns that stop a shared chain. Each test spreads
//! its threads over three separate handles on one file, so they contend through
//! SQLite's `BEGIN IMMEDIATE` locking rather than a shared connection mutex.
mod support;

use foundry_budget::{BudgetError, BudgetId, BudgetStore, ExhaustedCause, WallClock};
use std::sync::Barrier;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use support::concurrent::{atoms, temp_handles, with_watcher};
use support::{Rng, xts};
/// Spawns `threads` reservers on each budget in `targets`, each committing its
/// full maximum, and returns the total committed through each target.
/// Threads rotate across `stores`.
fn fill(stores: &[BudgetStore], targets: &[BudgetId], threads: u64, calls: u64) -> Vec<u64> {
    let totals: Vec<AtomicU64> = targets.iter().map(|_| AtomicU64::new(0)).collect();
    thread::scope(|scope| {
        for (index, &target) in targets.iter().enumerate() {
            for thread_index in 0..threads {
                let total = &totals[index];
                let store = &stores[(index + thread_index as usize) % stores.len()];
                scope.spawn(move || {
                    let mut rng = Rng::new(index as u64 * 1_000 + thread_index);
                    for _ in 0..calls {
                        let maximum = 1 + rng.below(150);
                        if let Ok(mut reservation) = store.reserve(target, atoms(maximum)) {
                            reservation.commit(atoms(maximum)).unwrap();
                            total.fetch_add(maximum, Ordering::SeqCst);
                        }
                    }
                });
            }
        }
    });
    totals.into_iter().map(AtomicU64::into_inner).collect()
}

#[test]
fn concurrent_children_are_capped_by_the_shared_parent() {
    const PARENT_CEILING: u64 = 9_000;
    const CHILD_CEILING: u64 = 3_000;
    let (_directory, handles) = temp_handles(3);
    let store = &handles[0];
    let parent = store
        .create(xts(), Some(atoms(PARENT_CEILING)), WallClock::NONE)
        .unwrap();
    let children: Vec<BudgetId> = (0..6)
        .map(|_| {
            store
                .delegate(parent, Some(atoms(CHILD_CEILING)), WallClock::NONE)
                .unwrap()
        })
        .collect();
    let mut watched = vec![(parent, u128::from(PARENT_CEILING))];
    watched.extend(
        children
            .iter()
            .map(|&child| (child, u128::from(CHILD_CEILING))),
    );
    let mut totals = Vec::new();
    with_watcher(&handles[2], &watched, || {
        totals = fill(&handles, &children, 4, 120)
    });
    let parent_snapshot = store.snapshot(parent).unwrap();
    let child_total: u64 = totals.iter().sum();
    assert_eq!(parent_snapshot.committed, atoms(child_total));
    assert_eq!(parent_snapshot.reserved, atoms(0));
    // Six children capped at 3,000 each could spend 18,000 alone; the parent's
    // 9,000 ceiling must hold, and the contention must actually reach it.
    assert!(child_total <= PARENT_CEILING);
    assert!(child_total > PARENT_CEILING - 150);
}

#[test]
fn a_grandchild_chain_holds_every_ceiling_under_contention() {
    let (_directory, handles) = temp_handles(3);
    let store = &handles[0];
    let root = store
        .create(xts(), Some(atoms(5_000)), WallClock::NONE)
        .unwrap();
    let child = store
        .delegate(root, Some(atoms(4_000)), WallClock::NONE)
        .unwrap();
    let grandchild = store
        .delegate(child, Some(atoms(3_000)), WallClock::NONE)
        .unwrap();
    let watched = [(root, 5_000), (child, 4_000), (grandchild, 3_000)];
    let mut totals = Vec::new();
    with_watcher(&handles[2], &watched, || {
        totals = fill(&handles, &[root, child, grandchild], 3, 120);
    });
    let committed = |budget| store.snapshot(budget).unwrap().committed;
    assert_eq!(committed(grandchild), atoms(totals[2]));
    assert_eq!(committed(child), atoms(totals[1] + totals[2]));
    assert_eq!(committed(root), atoms(totals.iter().sum()));
    assert!(
        totals.iter().sum::<u64>() > 5_000 - 150,
        "the root must be contended"
    );
}

#[test]
fn delegation_racing_reservation_never_widens_the_parent() {
    const CEILING: u64 = 5_000;
    const CHILD_CEILING: u64 = 400;
    let (_directory, handles) = temp_handles(3);
    let store = &handles[0];
    let parent = store
        .create(xts(), Some(atoms(CEILING)), WallClock::NONE)
        .unwrap();
    let (charged, delegated, refused) = (AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0));
    // Reservers use the first handle, delegators the second, the watcher the third.
    with_watcher(&handles[2], &[(parent, u128::from(CEILING))], || {
        thread::scope(|scope| {
            for seed in 0..4 {
                let (store, charged) = (&handles[0], &charged);
                scope.spawn(move || {
                    let mut rng = Rng::new(seed);
                    // At most 4 x 40 x 25 = 4,000 of the 5,000, so the first
                    // delegation always finds room and races these reservers.
                    for _ in 0..40 {
                        let maximum = 1 + rng.below(25);
                        if let Ok(mut reservation) = store.reserve(parent, atoms(maximum)) {
                            reservation.commit(atoms(maximum)).unwrap();
                            charged.fetch_add(maximum, Ordering::SeqCst);
                        }
                    }
                });
            }
            for _ in 0..4 {
                let (store, charged, delegated, refused) =
                    (&handles[1], &charged, &delegated, &refused);
                // Each delegator runs until the parent is too full for a child,
                // which its own spending guarantees.
                scope.spawn(move || {
                    for _ in 0..200 {
                        match store.delegate(parent, Some(atoms(CHILD_CEILING)), WallClock::NONE) {
                            Ok(child) => {
                                delegated.fetch_add(1, Ordering::SeqCst);
                                if let Ok(mut reservation) =
                                    store.reserve(child, atoms(CHILD_CEILING))
                                {
                                    reservation.commit(atoms(CHILD_CEILING)).unwrap();
                                    charged.fetch_add(CHILD_CEILING, Ordering::SeqCst);
                                }
                            }
                            Err(error) => {
                                let expected = BudgetError::InvalidRequest {
                                    field: "child_ceiling",
                                };
                                assert_eq!(error, expected);
                                refused.fetch_add(1, Ordering::SeqCst);
                                break;
                            }
                        }
                    }
                });
            }
        });
    });
    let snapshot = store.snapshot(parent).unwrap();
    assert_eq!(snapshot.committed, atoms(charged.into_inner()));
    assert!(snapshot.committed.atoms() <= u128::from(CEILING));
    assert!(delegated.into_inner() > 0);
    assert!(
        refused.into_inner() > 0,
        "delegation must race a nearly full parent"
    );
}

#[test]
fn concurrent_overruns_stop_the_whole_chain() {
    const THREADS: u64 = 16;
    const CEILING: u64 = 100_000;
    let (_directory, handles) = temp_handles(3);
    let store = &handles[0];
    let parent = store
        .create(xts(), Some(atoms(CEILING)), WallClock::NONE)
        .unwrap();
    let children: Vec<BudgetId> = (0..4)
        .map(|_| store.delegate(parent, None, WallClock::NONE).unwrap())
        .collect();
    let (admitted, excess) = (AtomicU64::new(0), AtomicU64::new(0));
    let start = Barrier::new(THREADS as usize);
    thread::scope(|scope| {
        for index in 0..THREADS {
            let store = &handles[index as usize % handles.len()];
            let (children, admitted, excess, start) = (&children, &admitted, &excess, &start);
            scope.spawn(move || {
                let child = children[index as usize % children.len()];
                start.wait();
                // Every commit overruns by 5, so the first one to land stops all.
                while let Ok(mut reservation) = store.reserve(child, atoms(100)) {
                    admitted.fetch_add(1, Ordering::SeqCst);
                    assert!(reservation.commit(atoms(105)).unwrap().overrun);
                    excess.fetch_add(5, Ordering::SeqCst);
                }
            });
        }
    });
    // Each thread holds at most one reservation when the first overrun lands.
    let admitted = admitted.into_inner();
    assert!((1..=THREADS).contains(&admitted), "admitted {admitted}");
    let snapshot = store.snapshot(parent).unwrap();
    assert!(snapshot.overrun);
    assert_eq!(snapshot.reserved, atoms(0));
    assert_eq!(snapshot.committed, atoms(admitted * 105));
    assert!(snapshot.committed.atoms() <= u128::from(CEILING + excess.into_inner()));
    for budget in children.iter().copied().chain([parent]) {
        match store.reserve(budget, atoms(1)).unwrap_err() {
            BudgetError::BudgetExhausted { cause, .. } => {
                assert_eq!(cause, ExhaustedCause::Overrun)
            }
            other => panic!("expected an overrun refusal, got {other}"),
        }
    }
}
