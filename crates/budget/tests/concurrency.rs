//! Concurrency tests against file-backed stores, so every reservation runs
//! through SQLite's `BEGIN IMMEDIATE` locking as well as the connection mutex.
mod support;

use foundry_budget::{BudgetId, BudgetStore, WallClock};
use std::sync::Barrier;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use support::concurrent::{atoms, open, temp_store, with_watcher};
use support::{Rng, money, xts};
/// Reserves at random and settles by drop (full charge), release, or a commit
/// at or below the maximum. Returns the total charged.
fn spend(store: &BudgetStore, budget: BudgetId, seed: u64, calls: u64, refused: &AtomicU64) -> u64 {
    let mut rng = Rng::new(seed);
    let mut charged = 0;
    for _ in 0..calls {
        let maximum = 1 + rng.below(400);
        let Ok(mut reservation) = store.reserve(budget, atoms(maximum)) else {
            refused.fetch_add(1, Ordering::SeqCst);
            continue;
        };
        match rng.below(4) {
            0 => {
                drop(reservation);
                charged += maximum;
            }
            1 => {
                reservation.release().unwrap();
            }
            _ => {
                let actual = rng.below(maximum + 1) / 8;
                reservation.commit(atoms(actual)).unwrap();
                charged += actual;
            }
        }
    }
    charged
}

#[test]
fn many_concurrent_reservers_never_overspend() {
    const CEILING: u64 = 6_000;
    let (_directory, store) = temp_store();
    let budget = store
        .create(xts(), Some(atoms(CEILING)), WallClock::NONE)
        .unwrap();
    let (charged, refused) = (AtomicU64::new(0), AtomicU64::new(0));
    with_watcher(&store, &[(budget, u128::from(CEILING))], || {
        thread::scope(|scope| {
            for seed in 0..8 {
                let (store, charged, refused) = (&store, &charged, &refused);
                scope.spawn(move || {
                    charged.fetch_add(spend(store, budget, seed, 150, refused), Ordering::SeqCst);
                });
            }
        });
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
fn two_connections_to_one_file_never_overspend() {
    const CEILING: u64 = 5_000;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("budget.sqlite");
    let (first, second, observer) = (open(&path), open(&path), open(&path));
    let budget = first
        .create(xts(), Some(atoms(CEILING)), WallClock::NONE)
        .unwrap();
    let (charged, refused) = (AtomicU64::new(0), AtomicU64::new(0));
    with_watcher(&observer, &[(budget, u128::from(CEILING))], || {
        thread::scope(|scope| {
            for seed in 0..8 {
                let store = if seed % 2 == 0 { &first } else { &second };
                let (charged, refused) = (&charged, &refused);
                scope.spawn(move || {
                    charged.fetch_add(spend(store, budget, seed, 120, refused), Ordering::SeqCst);
                });
            }
        });
    });
    let snapshot = observer.snapshot(budget).unwrap();
    assert_eq!(snapshot.reserved, atoms(0));
    assert_eq!(
        snapshot.committed.atoms(),
        u128::from(charged.load(Ordering::SeqCst))
    );
    assert!(snapshot.committed.atoms() <= u128::from(CEILING));
    assert!(
        refused.load(Ordering::SeqCst) > 0,
        "the ceiling must be contended"
    );
}

#[test]
fn exactly_one_concurrent_reservation_wins_when_one_fits() {
    const CONTENDERS: usize = 24;
    let (_directory, store) = temp_store();
    for _ in 0..30 {
        let budget = store
            .create(xts(), Some(money("1.00")), WallClock::NONE)
            .unwrap();
        store
            .reserve(budget, money("0.30"))
            .unwrap()
            .commit(money("0.30"))
            .unwrap();
        let (start, attempted) = (Barrier::new(CONTENDERS), Barrier::new(CONTENDERS));
        let winners = AtomicU64::new(0);
        thread::scope(|scope| {
            for _ in 0..CONTENDERS {
                scope.spawn(|| {
                    start.wait();
                    let result = store.reserve(budget, money("0.40"));
                    // Hold any winning reservation until every contender has tried.
                    attempted.wait();
                    match result {
                        Ok(mut held) => {
                            winners.fetch_add(1, Ordering::SeqCst);
                            held.release().unwrap();
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
fn cancelled_and_panicking_tasks_settle_their_reservations() {
    let (_directory, store) = temp_store();
    let budget = store
        .create(xts(), Some(atoms(1_000)), WallClock::NONE)
        .unwrap();
    let (held, observed) = (Barrier::new(9), Barrier::new(9));
    thread::scope(|scope| {
        let mut tasks = Vec::new();
        for index in 0..8 {
            let (store, held, observed) = (&store, &held, &observed);
            tasks.push(scope.spawn(move || {
                let mut reservation = store.reserve(budget, atoms(100)).unwrap();
                held.wait();
                observed.wait();
                if index % 2 == 0 {
                    // Unwinding drops the reservation, which charges it in full.
                    panic!("task {index} abandoned its call");
                }
                // A cancelled call that consumed nothing releases explicitly.
                reservation.release().unwrap();
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
        (atoms(400), atoms(0))
    );
    store
        .reserve(budget, atoms(600))
        .unwrap()
        .commit(atoms(600))
        .unwrap();
}
