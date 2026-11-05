//! Usage records and consumption reports from fixture attempts. Days are UTC
//! calendar days, as the provider specification requires.
mod usage_fixtures;

use foundry_budget::usage::{
    AttemptUsage, Bucket, GroupBy, GroupKey, Recording, UsageStore, UtcDate,
};
use foundry_budget::{Amount, BudgetStore, Clock, PriceTable, Usage, WallClock};
use std::sync::Arc;
use usage_fixtures::*;

#[test]
fn daily_lists_active_utc_days_oldest_first_and_excludes_the_end_day() {
    let (store, clock) = usage_store();
    record_all(
        &store,
        &clock,
        &[
            // The last millisecond before the window starts.
            (at("2026-01-01") - 1, attempt("before", "9", 1)),
            (at("2026-01-01"), attempt("a", "0.10", 20_000)),
            // The last millisecond of 2026-01-01 still belongs to it.
            (at("2026-01-02") - 1, attempt("b", "0.15", 40_000)),
            (
                at("2026-01-03") + 12 * HOUR_MS,
                attempt("c", "0.75", 120_000),
            ),
            // The end day is excluded, from its first millisecond.
            (at("2026-01-04"), attempt("after", "9", 1)),
        ],
    );

    let report = store
        .daily(Some(window("2026-01-01", "2026-01-04")), currency())
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
    assert_eq!(report.total.attempts, 3);
    assert_eq!(report.daily[0].totals.attempts, 2);
}

#[test]
fn a_retried_settlement_is_counted_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("budget.sqlite");
    let clock = Arc::new(FixtureClock::default());
    clock.set(at("2026-01-01") + HOUR_MS);
    let budgets = BudgetStore::open(&path, clock.clone()).unwrap();
    let usage = UsageStore::open(&path, clock.clone()).unwrap();

    let budget = budgets
        .create(xts(), Some(money("1")), WallClock::NONE)
        .unwrap();
    let mut reservation = budgets.reserve(budget, money("0.5")).unwrap();
    let settled = reservation.commit(money("0.3")).unwrap();
    let record = attempt("attempt-1", "0.3", 5_000);
    assert_eq!(usage.record(&record).unwrap(), Recording::Recorded);
    // The settlement is retried, for example after a lost acknowledgement:
    // the reservation refuses a second commit and the record is a no-op.
    assert_eq!(
        reservation.commit(money("0.3")).unwrap_err().class(),
        "conflict"
    );
    clock.set(clock.now_ms() + DAY_MS);
    assert_eq!(usage.record(&record).unwrap(), Recording::AlreadyRecorded);
    // A restarted process retrying the record still counts it once.
    drop(usage);
    let usage = UsageStore::open(&path, clock.clone()).unwrap();
    assert_eq!(usage.record(&record).unwrap(), Recording::AlreadyRecorded);

    let report = usage
        .daily(Some(window("2026-01-01", "2026-01-03")), currency())
        .unwrap();
    assert_eq!(report.total.spend, settled.committed);
    assert_eq!(report.total.attempts, 1);
    // The first recording's day is kept; the retry a day later moves nothing.
    assert_eq!(report.daily.len(), 1);
    assert_eq!(report.daily[0].date, UtcDate::parse("2026-01-01").unwrap());
    assert_eq!(budgets.snapshot(budget).unwrap().committed, money("0.3"));

    // Different content under the same attempt is refused and changes nothing.
    let changed = attempt("attempt-1", "0.4", 5_000);
    assert_eq!(usage.record(&changed).unwrap_err().class(), "conflict");
    let again = usage
        .daily(Some(window("2026-01-01", "2026-01-03")), currency())
        .unwrap();
    assert_eq!(again, report);
}

#[test]
fn grouping_by_session_orders_by_spend_with_unattributed_usage() {
    let (store, clock) = usage_store();
    let in_session = |id: &str, session: &str, cost: &str| AttemptUsage {
        session_id: Some(session.into()),
        ..attempt(id, cost, 1_000)
    };
    let day = at("2026-01-01");
    record_all(
        &store,
        &clock,
        &[
            (day, in_session("a", "session-1", "0.10")),
            (day + 1, attempt("b", "0.15", 1_000)),
            (day + 2, in_session("c", "session-2", "0.60")),
            (day + 3, attempt("d", "0.25", 1_000)),
            // Equal spend: the named group sorts first, then by identifier.
            (day + 4, in_session("e", "session-0", "0.40")),
        ],
    );
    let report = store
        .grouped(
            Some(window("2026-01-01", "2026-01-02")),
            currency(),
            GroupBy::Session,
        )
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
fn per_account_totals_split_by_provider_account() {
    let (store, clock) = usage_store();
    let on = |id: &str, account: &str, cost: &str| AttemptUsage {
        account_key: account.into(),
        user_id: Some("user-1".into()),
        ..attempt(id, cost, 1_000)
    };
    let day = at("2026-02-10");
    record_all(
        &store,
        &clock,
        &[
            (day, on("a", "primary", "0.2")),
            (day + 1, on("b", "secondary", "0.5")),
            (day + 2, on("c", "primary", "0.1")),
        ],
    );
    let range = Some(window("2026-02-01", "2026-03-01"));
    let report = store.grouped(range, currency(), GroupBy::Account).unwrap();
    let account = |key: &str| GroupKey::Account {
        provider_id: "fixture".into(),
        account_key: key.into(),
    };
    assert_eq!(report.items.len(), 2);
    assert_eq!(report.items[0].key, account("secondary"));
    assert_eq!(report.items[0].totals.spend, money("0.5"));
    assert_eq!(report.items[1].key, account("primary"));
    assert_eq!(report.items[1].totals.spend, money("0.3"));
    assert_eq!(report.items[1].totals.attempts, 2);
    assert_eq!(report.items[1].totals.usage.input_tokens, 200);
    let users = store.grouped(range, currency(), GroupBy::User).unwrap();
    assert_eq!(users.items.len(), 1);
    assert_eq!(users.items[0].key, GroupKey::User("user-1".into()));
    assert_eq!(users.total, report.total);
}

#[test]
fn metrics_report_integer_units_per_bucket_from_fixture_prices() {
    let (store, clock) = usage_store();
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
    let priced = AttemptUsage {
        usage,
        cost,
        ..attempt("a", "0", 0)
    };
    record_all(&store, &clock, &[(at("2026-01-01") + 30 * 60_000, priced)]);

    let range = Some(window("2026-01-01", "2026-01-02"));
    let days = store.metrics(range, Bucket::default()).unwrap();
    assert_eq!(days.len(), 1);
    assert_eq!(days[0].start_ms, at("2026-01-01"));
    assert_eq!(days[0].usage, usage);
    let hours = store.metrics(range, Bucket::Hour).unwrap();
    assert_eq!(hours[0].start_ms, at("2026-01-01"));
    // Reading reports records nothing.
    assert_eq!(store.metrics(range, Bucket::Day).unwrap(), days);
}

#[test]
fn the_default_window_is_the_last_thirty_utc_days_including_today() {
    let (store, clock) = usage_store();
    let today = at("2026-03-31") + 5 * HOUR_MS;
    record_all(
        &store,
        &clock,
        &[
            (at("2026-03-01") + 1, attempt("too-old", "1", 0)),
            (at("2026-03-02"), attempt("oldest", "0.1", 0)),
            (today, attempt("today", "0.2", 0)),
        ],
    );
    let report = store.daily(None, currency()).unwrap();
    assert_eq!(report.window, window("2026-03-02", "2026-04-01"));
    assert_eq!(report.window.days(), 30);
    assert_eq!(report.total.spend, money("0.3"));
}
