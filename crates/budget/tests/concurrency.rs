mod support;

use foundry_budget::{Amount, BudgetId, BudgetStore, MonotonicClock, WallClock};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use support::{Rng, money, xts};

fn store() -> BudgetStore {
    BudgetStore::new(Arc::new(MonotonicClock::new()))
}

fn atoms(value: u64) -> Amount {
    Amount::from_atoms(xts(), u128::from(value)).unwrap()
}

/// Samples `budgets` until `done`, asserting no ceiling is ever exceeded by
/// committed spend plus open reservations.
fn watch(store: &BudgetStore, budgets: &[(BudgetId, u128)], done: &AtomicBool) -> u64 {
    let mut samples = 0;
    while !done.load(Ordering::SeqCst) {
        for &(budget, ceiling) in budgets {
            let snapshot = store.snapshot(budget).unwrap();
            let used = snapshot.committed.atoms() + snapshot.reserved.atoms();
            assert!(
                used <= ceiling,
                "budget {budget:?} used {used} of {ceiling}"
            );
            samples += 1;
        }
    }
    samples
}

#[test]
fn many_concurrent_reservers_never_overspend() {
    const THREADS: u64 = 16;
    const CALLS: u64 = 400;
    const CEILING: u64 = 20_000;
    let store = store();
    let budget = store
        .open(xts(), Some(atoms(CEILING)), WallClock::NONE)
        .unwrap();
    let charged = AtomicU64::new(0);
    let refused = AtomicU64::new(0);
    let done = AtomicBool::new(false);
    thread::scope(|scope| {
        let watcher = scope.spawn(|| watch(&store, &[(budget, u128::from(CEILING))], &done));
        let workers: Vec<_> = (0..THREADS)
            .map(|seed| {
                let (store, charged, refused) = (&store, &charged, &refused);
                scope.spawn(move || {
                    let mut rng = Rng::new(seed);
                    for _ in 0..CALLS {
                        let maximum = 1 + rng.below(400);
                        let Ok(mut reservation) = store.reserve(budget, atoms(maximum)) else {
                            refused.fetch_add(1, Ordering::SeqCst);
                            continue;
                        };
                        match rng.below(4) {
                            0 => drop(reservation),
                            1 => {
                                reservation.release().unwrap();
                            }
                            _ => {
                                // Most calls end below their maximum; a few commit
                                // exactly the maximum. None overrun here.
                                let actual = rng.below(maximum + 1) / 8;
                                reservation.commit(atoms(actual)).unwrap();
                                charged.fetch_add(actual, Ordering::SeqCst);
                            }
                        }
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        done.store(true, Ordering::SeqCst);
        assert!(watcher.join().unwrap() > 0);
    });
    let snapshot = store.snapshot(budget).unwrap();
    assert_eq!(snapshot.reserved, atoms(0));
    assert_eq!(
        snapshot.committed.atoms(),
        u128::from(charged.load(Ordering::SeqCst))
    );
    assert!(snapshot.committed.atoms() <= u128::from(CEILING));
    assert!(!snapshot.overrun);
    assert!(
        refused.load(Ordering::SeqCst) > 0,
        "the ceiling must be contended"
    );
}

#[test]
fn exactly_one_concurrent_reservation_wins_when_one_fits() {
    const CONTENDERS: usize = 24;
    for _ in 0..50 {
        let store = store();
        let budget = store
            .open(xts(), Some(money("1.00")), WallClock::NONE)
            .unwrap();
        store
            .reserve(budget, money("0.30"))
            .unwrap()
            .commit(money("0.30"))
            .unwrap();
        let start = Barrier::new(CONTENDERS);
        let attempted = Barrier::new(CONTENDERS);
        let winners = AtomicU64::new(0);
        thread::scope(|scope| {
            for _ in 0..CONTENDERS {
                scope.spawn(|| {
                    start.wait();
                    let result = store.reserve(budget, money("0.40"));
                    // Hold any winning reservation until every contender has tried.
                    attempted.wait();
                    match result {
                        Ok(_held) => {
                            winners.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(error) => assert_eq!(error.class(), "budget_exhausted"),
                    }
                });
            }
        });
        assert_eq!(winners.load(Ordering::SeqCst), 1);
        assert_eq!(store.snapshot(budget).unwrap().reserved, money("0"));
    }
}

#[test]
fn concurrent_children_are_capped_by_the_shared_parent() {
    const CHILDREN: usize = 6;
    const THREADS_PER_CHILD: u64 = 4;
    const PARENT_CEILING: u64 = 9_000;
    const CHILD_CEILING: u64 = 3_000;
    let store = store();
    let parent = store
        .open(xts(), Some(atoms(PARENT_CEILING)), WallClock::NONE)
        .unwrap();
    let children: Vec<BudgetId> = (0..CHILDREN)
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
    let done = AtomicBool::new(false);
    thread::scope(|scope| {
        let watcher = scope.spawn(|| watch(&store, &watched, &done));
        let mut workers = Vec::new();
        for (index, &child) in children.iter().enumerate() {
            for thread_index in 0..THREADS_PER_CHILD {
                let store = &store;
                let seed = index as u64 * 100 + thread_index;
                workers.push(scope.spawn(move || {
                    let mut rng = Rng::new(seed);
                    for _ in 0..300 {
                        let maximum = 1 + rng.below(60);
                        if let Ok(mut reservation) = store.reserve(child, atoms(maximum)) {
                            reservation.commit(atoms(maximum)).unwrap();
                        }
                    }
                }));
            }
        }
        for worker in workers {
            worker.join().unwrap();
        }
        done.store(true, Ordering::SeqCst);
        watcher.join().unwrap();
    });
    let parent_snapshot = store.snapshot(parent).unwrap();
    let child_total: u128 = children
        .iter()
        .map(|&child| store.snapshot(child).unwrap().committed.atoms())
        .sum();
    assert_eq!(parent_snapshot.committed.atoms(), child_total);
    assert_eq!(parent_snapshot.reserved, atoms(0));
    // Six children capped at 3,000 each could spend 18,000 alone; the parent's
    // 9,000 ceiling must hold, and the contention must actually reach it.
    assert!(child_total <= u128::from(PARENT_CEILING));
    assert!(child_total > u128::from(PARENT_CEILING) - 60);
}

#[test]
fn reservations_held_by_cancelled_or_panicking_tasks_are_released() {
    let store = store();
    let budget = store
        .open(xts(), Some(atoms(1_000)), WallClock::NONE)
        .unwrap();
    let held = Barrier::new(9);
    let observed = Barrier::new(9);
    thread::scope(|scope| {
        let mut tasks = Vec::new();
        for index in 0..8 {
            let (store, held, observed) = (&store, &held, &observed);
            tasks.push(scope.spawn(move || {
                let _reservation = store.reserve(budget, atoms(100)).unwrap();
                held.wait();
                observed.wait();
                if index % 2 == 0 {
                    panic!("task {index} abandoned its call");
                }
            }));
        }
        held.wait();
        assert_eq!(store.snapshot(budget).unwrap().reserved, atoms(800));
        observed.wait();
        let panicked = tasks
            .into_iter()
            .filter_map(|task| task.join().err())
            .count();
        assert_eq!(panicked, 4);
    });
    let snapshot = store.snapshot(budget).unwrap();
    assert_eq!(
        (snapshot.committed, snapshot.reserved),
        (atoms(0), atoms(0))
    );
    store
        .reserve(budget, atoms(1_000))
        .unwrap()
        .commit(atoms(1_000))
        .unwrap();
}
