//! Metered settlements from several threads on separate connections to one
//! file still leave reports equal to committed spend, and an orphaned metered
//! reservation keeps its attempt on disk for a later reconcile.
mod usage_fixtures;

use foundry_budget::usage::{Attempt, GroupBy};
use foundry_budget::{BudgetError, BudgetStore, ManualClock, WallClock};
use rusqlite::Connection;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use usage_fixtures::*;

const THREADS: usize = 8;
const ROUNDS: usize = 30;

/// `Ok(true)` when the operation ran, `Ok(false)` once the chain refuses it.
fn run_op(
    store: &BudgetStore,
    child: foundry_budget::BudgetId,
    attempt: Attempt,
    op: usize,
    unmetered_atoms: &AtomicU64,
) -> Result<bool, BudgetError> {
    let refused = |error: BudgetError| match error {
        BudgetError::BudgetExhausted { .. } => Ok(false),
        other => Err(other),
    };
    let metered = |cost| store.reserve_attempt(child, money(cost), attempt.clone());
    match op {
        0 => match metered("0.03") {
            Ok(mut reservation) => reservation
                .commit_usage(money("0.02"), calls(5, 1))
                .map(|_| true),
            Err(error) => refused(error),
        },
        1 => match metered("0.03") {
            Ok(mut reservation) => reservation.release().map(|_| true),
            Err(error) => refused(error),
        },
        2 => match metered("0.01") {
            Ok(reservation) => {
                drop(reservation);
                Ok(true)
            }
            Err(error) => refused(error),
        },
        3 => match store.reserve(child, money("0.01")) {
            Ok(mut reservation) => {
                let settled = reservation.commit(money("0.01"))?;
                let atoms = u64::try_from(settled.charged.atoms()).unwrap();
                unmetered_atoms.fetch_add(atoms, Ordering::SeqCst);
                Ok(true)
            }
            Err(error) => refused(error),
        },
        // An overrun: it refuses every later reservation in the chain.
        _ => match metered("0.01") {
            Ok(mut reservation) => reservation
                .commit_usage(money("0.02"), calls(1, 1))
                .map(|_| true),
            Err(error) => refused(error),
        },
    }
}

#[test]
fn concurrent_metered_settlements_across_connections_match_committed_spend() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("budget.sqlite");
    let clock = Arc::new(ManualClock::default());
    let handles = [
        BudgetStore::open(&path, clock.clone()).unwrap(),
        BudgetStore::open(&path, clock.clone()).unwrap(),
    ];
    let root = handles[0].create(xts(), None, WallClock::NONE).unwrap();
    let children: Vec<_> = (0..THREADS)
        .map(|index| {
            handles[index % 2]
                .delegate(root, None, WallClock::NONE)
                .unwrap()
        })
        .collect();
    let unmetered_atoms = AtomicU64::new(0);

    thread::scope(|scope| {
        for (index, &child) in children.iter().enumerate() {
            let store = &handles[index % 2];
            let unmetered_atoms = &unmetered_atoms;
            scope.spawn(move || {
                let attempt = Attempt {
                    account_key: format!("account-{}", index % 3),
                    ..attempt(&format!("attempt-{index}"))
                };
                for round in 0..ROUNDS {
                    // Thread 0 overruns on its last round; the others keep
                    // going until the shared chain refuses them.
                    let op = if index == 0 && round == ROUNDS - 1 {
                        4
                    } else {
                        (index + round) % 4
                    };
                    if !run_op(store, child, attempt.clone(), op, unmetered_atoms).unwrap() {
                        break;
                    }
                }
            });
        }
    });

    let root_snapshot = handles[1].snapshot(root).unwrap();
    assert!(root_snapshot.overrun);
    assert_eq!(root_snapshot.reserved, money("0"));
    let range = window("1970-01-01", "1970-01-02");
    let accounts = handles[1]
        .usage()
        .grouped(range, currency(), GroupBy::Account)
        .unwrap();
    assert_eq!(accounts.items.len(), 3);
    let unmetered = foundry_budget::Amount::from_atoms(
        xts(),
        u128::from(unmetered_atoms.load(Ordering::SeqCst)),
    )
    .unwrap();
    let per_account = accounts
        .items
        .iter()
        .try_fold(unmetered, |sum, item| sum.checked_add(item.totals.spend))
        .unwrap();
    assert_eq!(per_account, root_snapshot.committed);
    assert_eq!(
        accounts.total.spend.checked_add(unmetered).unwrap(),
        root_snapshot.committed
    );
    // Both connections read the same rows.
    let other = handles[0]
        .usage()
        .grouped(range, currency(), GroupBy::Account)
        .unwrap();
    assert_eq!(other, accounts);
}

#[test]
fn an_orphaned_metered_reservation_keeps_its_attempt_on_disk() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("budget.sqlite");
    let clock = Arc::new(ManualClock::default());
    let store = BudgetStore::open(&path, clock.clone()).unwrap();
    let budget = store.create(xts(), None, WallClock::NONE).unwrap();
    let orphan = Attempt {
        session_id: Some("session-1".into()),
        ..attempt("orphan")
    };
    // A crash while the call is in flight: the handle never settles.
    std::mem::forget(store.reserve_attempt(budget, money("0.4"), orphan).unwrap());
    let mut settled = store
        .reserve_attempt(budget, money("0.1"), attempt("settled"))
        .unwrap();
    settled.commit_usage(money("0.1"), calls(1, 0)).unwrap();
    drop(store);

    let connection = Connection::open(&path).unwrap();
    let rows: Vec<(String, Option<String>, String)> = connection
        .prepare(
            "SELECT a.attempt_id, a.session_id, r.atoms
             FROM reservation_attempts a JOIN reservations r ON r.id = a.reservation",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    // Only the open reservation's attempt remains; the settled one was removed.
    assert_eq!(
        rows,
        [(
            "orphan".into(),
            Some("session-1".into()),
            "400000000".into()
        )]
    );
    drop(connection);

    let reopened = BudgetStore::open(&path, clock).unwrap();
    assert_eq!(reopened.snapshot(budget).unwrap().reserved, money("0.4"));
    let report = reopened
        .usage()
        .daily(window("1970-01-01", "1970-01-02"), currency())
        .unwrap();
    assert_eq!(report.total.spend, money("0.1"));
}
