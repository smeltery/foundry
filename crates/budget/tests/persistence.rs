//! The budget database survives a restart: each test drops every handle and
//! reopens the file, as a new process would.
mod support;

use foundry_budget::{BudgetStore, DeadlineState, ManualClock, WallClock};
use std::path::Path;
use std::sync::Arc;
use support::{money, xts};

fn reopen(path: &Path, clock: &Arc<ManualClock>) -> BudgetStore {
    BudgetStore::open(path, clock.clone()).unwrap()
}

#[test]
fn spend_open_reservations_and_deadlines_survive_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("budget.sqlite");
    let clock = Arc::new(ManualClock::default());
    clock.advance(1_000_000);
    let wall_clock = WallClock::from_ms(120_000).unwrap();

    let store = reopen(&path, &clock);
    let budget = store
        .create(xts(), Some(money("1.00")), wall_clock)
        .unwrap();
    store
        .reserve(budget, money("0.60"))
        .unwrap()
        .commit(money("0.60"))
        .unwrap();
    // A crash while a call is in flight: the reservation is never settled.
    std::mem::forget(store.reserve(budget, money("0.10")).unwrap());
    drop(store);

    clock.advance(60_000);
    let restarted = reopen(&path, &clock);
    let snapshot = restarted.snapshot(budget).unwrap();
    assert_eq!(snapshot.committed, money("0.60"));
    assert_eq!(snapshot.reserved, money("0.10"));
    assert_eq!(snapshot.remaining, Some(money("0.30")));
    assert_eq!(snapshot.deadline, DeadlineState::Remaining { ms: 60_000 });
    // Without persistence the restarted process would see a fresh 1.00.
    let refused = restarted.reserve(budget, money("0.31")).unwrap_err();
    assert_eq!(refused.class(), "budget_exhausted");
    restarted
        .reserve(budget, money("0.30"))
        .unwrap()
        .commit(money("0.30"))
        .unwrap();
    drop(restarted);

    // The deadline counts from the original admission, not from the reopen.
    clock.advance(60_000);
    let late = reopen(&path, &clock);
    assert_eq!(
        late.snapshot(budget).unwrap().deadline,
        DeadlineState::Exceeded
    );
    assert_eq!(late.snapshot(budget).unwrap().committed, money("0.90"));
    assert_eq!(
        late.reserve(budget, money("0")).unwrap_err().class(),
        "budget_exhausted"
    );
}

#[test]
fn delegation_and_overruns_survive_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("budget.sqlite");
    let clock = Arc::new(ManualClock::default());
    let store = reopen(&path, &clock);
    let parent = store
        .create(xts(), Some(money("1")), WallClock::NONE)
        .unwrap();
    let child = store
        .delegate(parent, Some(money("0.5")), WallClock::NONE)
        .unwrap();
    let sibling = store.delegate(parent, None, WallClock::NONE).unwrap();
    store
        .reserve(child, money("0.2"))
        .unwrap()
        .commit(money("0.3"))
        .unwrap();
    drop(store);

    let restarted = reopen(&path, &clock);
    let snapshot = restarted.snapshot(child).unwrap();
    assert_eq!(snapshot.parent, Some(parent));
    assert_eq!(snapshot.ceiling, Some(money("0.5")));
    assert!(snapshot.overrun);
    assert_eq!(restarted.snapshot(parent).unwrap().committed, money("0.3"));
    let refused = restarted.reserve(sibling, money("0.1")).unwrap_err();
    assert_eq!(refused.class(), "budget_exhausted");
    let widened = restarted.delegate(parent, Some(money("0.8")), WallClock::NONE);
    assert_eq!(widened.unwrap_err().class(), "invalid_request");
}

#[test]
fn the_database_uses_wal_and_its_own_schema_version() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("budget.sqlite");
    let clock = Arc::new(ManualClock::default());
    drop(reopen(&path, &clock));

    let connection = rusqlite::Connection::open(&path).unwrap();
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let version: i64 = connection
        .query_row("SELECT version FROM budget_schema_version", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(version, 1);
    // Reopening an up-to-date file keeps its data and does not migrate again.
    drop(connection);
    let store = reopen(&path, &clock);
    let budget = store.create(xts(), None, WallClock::NONE).unwrap();
    drop(store);
    reopen(&path, &clock).snapshot(budget).unwrap();

    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute("UPDATE budget_schema_version SET version = 2", [])
        .unwrap();
    drop(connection);
    let error = BudgetStore::open(&path, clock).unwrap_err();
    assert_eq!(error.class(), "unavailable");
    assert!(error.to_string().contains("unsupported schema version 2"));
}
