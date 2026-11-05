//! Usage rows are written by settlement itself, so reports always agree with
//! committed spend. Days are UTC calendar days of the settlement time.
mod usage_fixtures;

use foundry_budget::usage::{Attempt, Bucket, GroupBy, GroupKey, UtcDate};
use foundry_budget::{Amount, BudgetStore, CostUnit, PriceTable, Usage, WallClock};
use std::sync::Arc;
use usage_fixtures::*;

#[test]
fn daily_lists_active_utc_days_oldest_first_and_excludes_the_end_day() {
    let (store, clock, budget) = fixture_store();
    let charge = |time, id: &str, cost: &str, compute_ms| {
        settle(
            &store,
            &clock,
            budget,
            time,
            attempt(id),
            money(cost),
            calls(100, compute_ms),
        );
    };
    // The last millisecond before the window starts.
    charge(at("2026-01-01") - 1, "before", "9", 1);
    charge(at("2026-01-01"), "a", "0.10", 20_000);
    // The last millisecond of 2026-01-01 still belongs to it.
    charge(at("2026-01-02") - 1, "b", "0.15", 40_000);
    charge(at("2026-01-03") + 12 * HOUR_MS, "c", "0.75", 120_000);
    // The end day is excluded, from its first millisecond.
    charge(at("2026-01-04"), "after", "9", 1);

    let report = store
        .usage()
        .daily(window("2026-01-01", "2026-01-04"), currency())
        .unwrap();
    let days: Vec<(String, String, u64)> = report
        .daily
        .iter()
        .map(|entry| {
            let totals = entry.totals;
            (
                entry.date.to_string(),
                totals.spend.to_string(),
                totals.usage.compute_ms,
            )
        })
        .collect();
    assert_eq!(
        days,
        [
            ("2026-01-01".into(), "0.25".into(), 60_000),
            ("2026-01-03".into(), "0.75".into(), 120_000),
        ]
    );
    assert_eq!(report.total.spend, money("1.00"));
    assert_eq!(report.total.usage.compute_ms, 180_000);
    assert_eq!(report.total.settlements, 3);
}

#[test]
fn a_retried_settlement_is_counted_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("budget.sqlite");
    let clock = Arc::new(FixtureClock::default());
    clock.set(at("2026-01-01") + HOUR_MS);
    let store = BudgetStore::open(&path, clock.clone()).unwrap();
    let budget = store
        .create(xts(), Some(money("1")), WallClock::NONE)
        .unwrap();

    let mut reservation = store
        .reserve_attempt(budget, money("0.5"), attempt("attempt-1"))
        .unwrap();
    let settled = reservation
        .commit_usage(money("0.3"), calls(7, 5_000))
        .unwrap();
    // The settlement is retried, for example after a lost acknowledgement.
    // The reservation is gone, so neither the ledger nor the report changes.
    clock.set(at("2026-01-02"));
    let retried = reservation.commit_usage(money("0.3"), calls(7, 5_000));
    assert_eq!(retried.unwrap_err().class(), "conflict");
    assert_eq!(reservation.release().unwrap_err().class(), "conflict");
    drop(reservation);
    drop(store);

    // A restarted process sees the one row.
    let store = BudgetStore::open(&path, clock).unwrap();
    let range = window("2026-01-01", "2026-01-03");
    let report = store.usage().daily(range, currency()).unwrap();
    assert_eq!(report.total.spend, settled.charged);
    assert_eq!(report.total.settlements, 1);
    assert_eq!(report.total.usage, calls(7, 5_000));
    assert_eq!(report.daily.len(), 1);
    assert_eq!(report.daily[0].date, UtcDate::parse("2026-01-01").unwrap());
    assert_eq!(store.snapshot(budget).unwrap().committed, settled.committed);
}

#[test]
fn the_settlement_time_fixes_the_day() {
    let (store, clock, budget) = fixture_store();
    clock.set(at("2026-01-02") - 1);
    let mut reservation = store
        .reserve_attempt(budget, money("1"), attempt("late"))
        .unwrap();
    clock.set(at("2026-01-02") + 1);
    reservation.commit(money("0.4")).unwrap();
    let range = window("2026-01-01", "2026-01-03");
    let report = store.usage().daily(range, currency()).unwrap();
    assert_eq!(report.daily.len(), 1);
    assert_eq!(report.daily[0].date, UtcDate::parse("2026-01-02").unwrap());
}

#[test]
fn a_dropped_reservation_is_reported_at_its_full_charge() {
    let (store, clock, budget) = fixture_store();
    clock.set(at("2026-01-01"));
    let cancelled = store
        .reserve_attempt(budget, money("0.50"), attempt("cancelled"))
        .unwrap();
    drop(cancelled);

    let committed = store.snapshot(budget).unwrap().committed;
    assert_eq!(committed, money("0.50"));
    let range = window("2026-01-01", "2026-01-02");
    let accounts = store
        .usage()
        .grouped(range, currency(), GroupBy::Account)
        .unwrap();
    assert_eq!(accounts.items.len(), 1);
    assert_eq!(accounts.items[0].totals.spend, committed);
    // The dropped call's token counts are unknown.
    assert_eq!(accounts.items[0].totals.usage, Usage::default());
}

#[test]
fn per_account_totals_match_committed_spend() {
    let (store, clock, budget) = fixture_store();
    clock.set(at("2026-02-10"));
    let on = |id: &str, account: &str| Attempt {
        account_key: account.into(),
        ..attempt(id)
    };
    let reserve = |id, account, cost| store.reserve_attempt(budget, money(cost), on(id, account));
    // A release, a drop, unmetered spend, then two calls of one attempt where
    // the last overruns (an overrun refuses any later reservation).
    reserve("b", "secondary", "0.3").unwrap().release().unwrap();
    drop(reserve("c", "secondary", "0.25").unwrap());
    // Unmetered spend moves the ledger but writes no usage row.
    let mut unmetered = store.reserve(budget, money("0.05")).unwrap();
    let counted = unmetered.commit_usage(money("0.05"), calls(1, 0));
    assert_eq!(counted.unwrap_err().class(), "invalid_request");
    unmetered.commit(money("0.05")).unwrap();
    let mut first = reserve("a", "primary", "0.2").unwrap();
    first.commit_usage(money("0.2"), calls(100, 0)).unwrap();
    let mut second = reserve("a", "primary", "0.1").unwrap();
    assert!(
        second
            .commit_usage(money("0.15"), calls(50, 0))
            .unwrap()
            .overrun
    );

    let usage = store.usage();
    let range = window("2026-02-01", "2026-03-01");
    let accounts = usage.grouped(range, currency(), GroupBy::Account).unwrap();
    let account = |key: &str| GroupKey::Account {
        provider_id: "fixture".into(),
        account_key: key.into(),
    };
    let spends: Vec<(GroupKey, Amount, u64)> = accounts
        .items
        .iter()
        .map(|item| (item.key.clone(), item.totals.spend, item.totals.settlements))
        .collect();
    assert_eq!(
        spends,
        [
            (account("primary"), money("0.35"), 2),
            (account("secondary"), money("0.25"), 2),
        ]
    );
    let committed = store.snapshot(budget).unwrap().committed;
    let metered = accounts.total.spend;
    assert_eq!(metered.checked_add(money("0.05")).unwrap(), committed);

    let attempts = usage.grouped(range, currency(), GroupBy::Attempt).unwrap();
    assert_eq!(attempts.items[0].key, GroupKey::Attempt("a".into()));
    assert_eq!(attempts.items[0].totals.usage.input_tokens, 150);
    assert_eq!(attempts.total, accounts.total);
}

#[test]
fn a_failed_settlement_writes_no_usage() {
    let (store, clock, budget) = fixture_store();
    clock.set(at("2026-01-01"));
    let mut reservation = store
        .reserve_attempt(budget, money("1"), attempt("a"))
        .unwrap();
    let tokens = Amount::from_atoms(CostUnit::InputTokens, 1).unwrap();
    let refused = reservation.commit(tokens).unwrap_err();
    assert_eq!(refused.class(), "invalid_request");
    let range = window("2026-01-01", "2026-01-02");
    assert!(
        store
            .usage()
            .metrics(range, Bucket::Day)
            .unwrap()
            .is_empty()
    );
    // The reservation stayed open; the corrected retry writes exactly one row.
    reservation.commit(money("0.6")).unwrap();
    let report = store.usage().daily(range, currency()).unwrap();
    assert_eq!(report.total.settlements, 1);
    assert_eq!(
        report.total.spend,
        store.snapshot(budget).unwrap().committed
    );
}

#[test]
fn grouping_by_session_orders_by_spend_with_unattributed_usage() {
    let (store, clock, budget) = fixture_store();
    let charge = |id: &str, session: Option<&str>, cost: &str| {
        let attempt = Attempt {
            session_id: session.map(Into::into),
            ..attempt(id)
        };
        let time = at("2026-01-01");
        settle(
            &store,
            &clock,
            budget,
            time,
            attempt,
            money(cost),
            calls(1, 0),
        );
    };
    charge("a", Some("session-1"), "0.10");
    charge("b", None, "0.15");
    charge("c", Some("session-2"), "0.60");
    charge("d", None, "0.25");
    // Equal spend: the named group sorts first, then by identifier.
    charge("e", Some("session-0"), "0.40");
    let range = window("2026-01-01", "2026-01-02");
    let report = store
        .usage()
        .grouped(range, currency(), GroupBy::Session)
        .unwrap();
    let items: Vec<(GroupKey, Amount)> = report
        .items
        .iter()
        .map(|item| (item.key.clone(), item.totals.spend))
        .collect();
    assert_eq!(
        items,
        [
            (GroupKey::Session("session-2".into()), money("0.60")),
            (GroupKey::Session("session-0".into()), money("0.40")),
            (GroupKey::Unattributed, money("0.40")),
            (GroupKey::Session("session-1".into()), money("0.10")),
        ]
    );
    assert_eq!(report.total.spend, money("1.50"));
}

#[test]
fn metrics_report_integer_units_per_bucket_from_fixture_prices() {
    let (store, clock, budget) = fixture_store();
    let usage = Usage {
        input_tokens: 1_200,
        output_tokens: 300,
        compute_ms: 60_000,
    };
    let cost = PriceTable::fixture()
        .cost("fixture", "fixture-small", xts(), usage)
        .unwrap();
    // 1,200 tokens at 1,000 nano-units plus 300 at 4,000.
    assert_eq!(cost, money("0.0024"));
    let time = at("2026-01-01") + 30 * 60_000;
    settle(&store, &clock, budget, time, attempt("a"), cost, usage);

    let range = window("2026-01-01", "2026-01-02");
    let days = store.usage().metrics(range, Bucket::default()).unwrap();
    assert_eq!(days.len(), 1);
    assert_eq!(days[0].start_ms, at("2026-01-01"));
    assert_eq!(days[0].usage, usage);
    let hours = store.usage().metrics(range, Bucket::Hour).unwrap();
    assert_eq!(hours[0].start_ms, at("2026-01-01"));
    let spend = store.usage().daily(range, currency()).unwrap().total.spend;
    assert_eq!(spend, cost);
}

#[test]
fn the_default_window_is_the_last_thirty_utc_days_including_today() {
    let (store, clock, budget) = fixture_store();
    let charge = |time, id: &str, cost: &str| {
        settle(
            &store,
            &clock,
            budget,
            time,
            attempt(id),
            money(cost),
            Usage::default(),
        );
    };
    charge(at("2026-03-01") + 1, "too-old", "1");
    charge(at("2026-03-02"), "oldest", "0.1");
    charge(at("2026-03-31") + 5 * HOUR_MS, "today", "0.2");
    let report = store.usage().daily(None, currency()).unwrap();
    assert_eq!(Some(report.window), window("2026-03-02", "2026-04-01"));
    assert_eq!(report.window.days(), 30);
    assert_eq!(report.total.spend, money("0.3"));
}
