//! A budget database written at schema version 1, before usage records existed,
//! is upgraded in place by schema step 2 and keeps its budgets.
mod support;

use foundry_budget::usage::{Attempt, UtcDate, Window};
use foundry_budget::{BudgetStore, Currency, ManualClock, Usage, WallClock};
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;
use support::{money, xts};

/// The version 1 schema exactly as the first budget release created it.
const VERSION_1: &str = "
CREATE TABLE budget_schema_version (version INTEGER NOT NULL);
CREATE TABLE budgets (
    id INTEGER PRIMARY KEY,
    parent INTEGER REFERENCES budgets(id),
    unit TEXT NOT NULL,
    currency TEXT,
    ceiling TEXT,
    committed TEXT NOT NULL,
    reserved TEXT NOT NULL,
    overrun INTEGER NOT NULL,
    admitted_at_ms INTEGER NOT NULL,
    wall_clock_ms INTEGER
);
CREATE TABLE reservations (
    id INTEGER PRIMARY KEY,
    budget INTEGER NOT NULL REFERENCES budgets(id),
    atoms TEXT NOT NULL
);
INSERT INTO budget_schema_version (version) VALUES (1);
PRAGMA application_id = 1178747975; -- 0x4642_4447
INSERT INTO budgets VALUES (1, NULL, 'money', 'XTS', '1000000000', '400000000', '100000000', 0, 0, NULL);
INSERT INTO reservations VALUES (1, 1, '100000000');
";

fn write_version_1(path: &Path, extra: &str) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch("PRAGMA journal_mode=WAL;")
        .unwrap();
    connection.execute_batch(VERSION_1).unwrap();
    connection.execute_batch(extra).unwrap();
}

fn version(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT version FROM budget_schema_version", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn tables(path: &Path) -> Vec<String> {
    let connection = Connection::open(path).unwrap();
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Every budget and reservation row, as stored.
fn ledger_rows(path: &Path) -> Vec<String> {
    let connection = Connection::open(path).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT 'budget', id, parent, unit, currency, ceiling, committed, reserved, overrun,
                    admitted_at_ms, wall_clock_ms FROM budgets
             UNION ALL
             SELECT 'reservation', id, budget, atoms, NULL, NULL, NULL, NULL, NULL, NULL, NULL
             FROM reservations ORDER BY 1, 2",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            let values: Vec<String> = (0..11)
                .map(|index| format!("{:?}", row.get_ref(index).unwrap()))
                .collect();
            Ok(values.join("|"))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn a_version_1_database_is_upgraded_and_keeps_its_budgets() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("budget.sqlite");
    write_version_1(&path, "");
    assert_eq!(version(&path), 1);
    assert!(!tables(&path).contains(&"usage_records".to_owned()));
    let before = ledger_rows(&path);
    assert_eq!(before.len(), 2);

    let clock = Arc::new(ManualClock::default());
    let store = BudgetStore::open(&path, clock.clone()).unwrap();
    assert_eq!(version(&path), 2);
    assert!(tables(&path).contains(&"usage_records".to_owned()));
    assert_eq!(ledger_rows(&path), before);

    // The upgraded file continues its row IDs and meters new settlements.
    let budget = store.create(xts(), None, WallClock::NONE).unwrap();
    assert_eq!(budget.get(), 2);
    let attempt = Attempt {
        attempt_id: "attempt-1".into(),
        provider_id: "fixture".into(),
        account_key: "primary".into(),
        model: "fixture-small".into(),
        session_id: None,
        user_id: None,
    };
    store
        .reserve_attempt(budget, money("0.5"), attempt)
        .unwrap()
        .commit_usage(money("0.4"), Usage::default())
        .unwrap();
    drop(store);

    // Reopening an upgraded file runs no step again and keeps the row.
    let reopened = BudgetStore::open(&path, clock).unwrap();
    assert_eq!(version(&path), 2);
    let window = Window::new(
        UtcDate::parse("1970-01-01").unwrap(),
        UtcDate::parse("1970-01-02").unwrap(),
    )
    .unwrap();
    let report = reopened
        .usage()
        .daily(Some(window), Currency::parse("XTS").unwrap())
        .unwrap();
    assert_eq!(report.total.spend, money("0.4"));
    assert_eq!(reopened.snapshot(budget).unwrap().committed, money("0.4"));
}

#[test]
fn a_failed_step_leaves_the_version_1_file_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("budget.sqlite");
    // A stray table under the step's name makes the step fail part way.
    write_version_1(&path, "CREATE TABLE usage_records (stray INTEGER);");

    let error = BudgetStore::open(&path, Arc::new(ManualClock::default())).unwrap_err();
    assert_eq!(error.class(), "unavailable");
    assert!(
        error.to_string().contains("usage_records already exists"),
        "{error}"
    );
    assert_eq!(version(&path), 1);
    let columns: Vec<String> = {
        let connection = Connection::open(&path).unwrap();
        let mut statement = connection
            .prepare("SELECT name FROM pragma_table_info('usage_records')")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(columns, ["stray"]);
}
