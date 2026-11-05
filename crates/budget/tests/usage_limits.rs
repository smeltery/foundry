//! Report windows, checked sums, currency filtering, attempt validation and
//! order independence for usage metering.
mod support;
mod usage_fixtures;

use foundry_budget::usage::{Attempt, Bucket, GroupBy, UtcDate, Window};
use foundry_budget::{Amount, CostUnit, Currency, Usage, WallClock};
use support::Rng;
use usage_fixtures::*;

#[test]
fn windows_are_bounded_and_ordered() {
    let date = |text| UtcDate::parse(text).unwrap();
    let invalid = |start, end| Window::new(date(start), date(end)).unwrap_err().class();
    assert_eq!(invalid("2026-01-04", "2026-01-01"), "invalid_request");
    assert_eq!(invalid("2026-01-01", "2026-01-01"), "invalid_request");
    // 2024 is a leap year: 2024-01-01 to 2025-01-02 spans 367 days.
    assert_eq!(invalid("2024-01-01", "2025-01-02"), "invalid_request");
    assert_eq!(window("2024-01-01", "2025-01-01").unwrap().days(), 366);
    let too_long = Window::last_days(at("2026-01-01"), 367).unwrap_err();
    assert_eq!(too_long.class(), "invalid_request");

    for valid in [
        "1970-01-01",
        "2000-02-29",
        "2024-02-29",
        "2026-12-31",
        "9999-12-31",
    ] {
        assert_eq!(date(valid).to_string(), valid);
    }
    for bad in [
        "2025-02-29",
        "1900-02-28",
        "2026-13-01",
        "2026-1-01",
        "2026-01-01Z",
        "+026-01-01",
    ] {
        assert_eq!(UtcDate::parse(bad).unwrap_err().class(), "invalid_request");
    }
    assert_eq!(date("1970-01-02").start_ms(), DAY_MS);
    assert_eq!(UtcDate::containing(DAY_MS - 1), date("1970-01-01"));
}

#[test]
fn overflowing_stored_sums_are_unavailable_and_never_wrap() {
    let (store, clock, budget) = fixture_store();
    let huge = Usage {
        input_tokens: u64::MAX,
        ..Usage::default()
    };
    settle(&store, &clock, budget, 1, attempt("a"), money("0"), huge);
    settle(&store, &clock, budget, 2, attempt("b"), money("0"), huge);
    let range = window("1970-01-01", "1970-01-02");
    let usage = store.usage();
    assert_eq!(
        usage.daily(range, currency()).unwrap_err().class(),
        "unavailable"
    );
    let metrics = usage.metrics(range, Bucket::Day).unwrap_err();
    assert_eq!(
        metrics.to_string(),
        "unavailable: budget store: usage total overflows"
    );

    // Each budget's own ledger holds its maximum; only the report sum overflows.
    let (store, clock, _) = fixture_store();
    let max_money = Amount::from_atoms(xts(), u128::MAX).unwrap();
    for id in ["a", "b"] {
        let budget = store.create(xts(), None, WallClock::NONE).unwrap();
        settle(
            &store,
            &clock,
            budget,
            1,
            attempt(id),
            max_money,
            Usage::default(),
        );
    }
    let grouped = store.usage().grouped(range, currency(), GroupBy::Account);
    assert_eq!(grouped.unwrap_err().class(), "unavailable");
}

#[test]
fn spend_reports_cover_only_the_requested_currency() {
    let (store, clock, budget) = fixture_store();
    let other = Currency::parse("XXX").unwrap();
    let foreign = store
        .create(CostUnit::Money(other), None, WallClock::NONE)
        .unwrap();
    let tokens = store
        .create(CostUnit::InputTokens, None, WallClock::NONE)
        .unwrap();
    settle(
        &store,
        &clock,
        budget,
        1,
        attempt("a"),
        money("1"),
        calls(1, 0),
    );
    let foreign_cost = Amount::parse(CostUnit::Money(other), "2").unwrap();
    settle(
        &store,
        &clock,
        foreign,
        2,
        attempt("b"),
        foreign_cost,
        calls(1, 0),
    );
    let token_cost = Amount::from_atoms(CostUnit::InputTokens, 500).unwrap();
    settle(
        &store,
        &clock,
        tokens,
        3,
        attempt("c"),
        token_cost,
        calls(500, 0),
    );

    let range = window("1970-01-01", "1970-01-02");
    let usage = store.usage();
    let in_xts = usage.daily(range, currency()).unwrap();
    assert_eq!(
        (in_xts.total.spend, in_xts.total.settlements),
        (money("1"), 1)
    );
    let in_other = usage.daily(range, other).unwrap();
    assert_eq!(in_other.total.spend, foreign_cost);
    assert_eq!(in_other.currency, other);
    // Metrics carry no spend, so they count every settlement.
    let metrics = usage.metrics(range, Bucket::Day).unwrap();
    assert_eq!(metrics[0].settlements, 3);
    assert_eq!(metrics[0].usage.input_tokens, 502);
}

#[test]
fn malformed_attempts_are_refused_before_reserving() {
    let (store, _clock, budget) = fixture_store();
    let cases: Vec<(Attempt, &str)> = vec![
        (attempt(""), "attempt_id"),
        (attempt(&"x".repeat(257)), "attempt_id"),
        (
            Attempt {
                provider_id: "p".repeat(65),
                ..attempt("a")
            },
            "provider_id",
        ),
        (
            Attempt {
                account_key: String::new(),
                ..attempt("a")
            },
            "account_key",
        ),
        (
            Attempt {
                model: "bad\0model".into(),
                ..attempt("a")
            },
            "model",
        ),
        (
            Attempt {
                session_id: Some(String::new()),
                ..attempt("a")
            },
            "session_id",
        ),
        (
            Attempt {
                user_id: Some("u".repeat(257)),
                ..attempt("a")
            },
            "user_id",
        ),
    ];
    for (attempt, field) in cases {
        let error = store
            .reserve_attempt(budget, money("1"), attempt)
            .unwrap_err();
        assert_eq!(error.to_string(), format!("invalid_request: {field}"));
    }
    assert_eq!(store.snapshot(budget).unwrap().reserved, money("0"));
}

#[test]
fn reports_do_not_depend_on_settlement_order() {
    let base = at("2026-01-01");
    // Each fixture settlement has a fixed time spread over a week.
    let fixtures: Vec<(u64, Attempt, Amount)> = (0..40u64)
        .map(|index| {
            let attempt = Attempt {
                account_key: format!("account-{}", index % 3),
                session_id: (index % 4 != 0).then(|| format!("session-{}", index % 5)),
                ..attempt(&format!("attempt-{:02}", index % 9))
            };
            let time = base + (index % 7) * DAY_MS + index * HOUR_MS / 2;
            (time, attempt, money(&format!("0.{index:02}")))
        })
        .collect();
    let range = window("2026-01-01", "2026-01-09");
    let mut expected = None;
    for seed in 0..8 {
        let mut order = fixtures.clone();
        let mut rng = Rng::new(seed);
        for index in (1..order.len()).rev() {
            order.swap(index, rng.below(index as u64 + 1) as usize);
        }
        let (store, clock, budget) = fixture_store();
        for (time, attempt, cost) in order {
            settle(
                &store,
                &clock,
                budget,
                time,
                attempt,
                cost,
                calls(time % 97, 1),
            );
        }
        let usage = store.usage();
        let reports = (
            usage.daily(range, currency()).unwrap(),
            usage.grouped(range, currency(), GroupBy::Session).unwrap(),
            usage.grouped(range, currency(), GroupBy::Account).unwrap(),
            usage.grouped(range, currency(), GroupBy::Attempt).unwrap(),
            usage.metrics(range, Bucket::Hour).unwrap(),
        );
        match &expected {
            None => expected = Some(reports),
            Some(expected) => assert_eq!(expected, &reports, "seed {seed}"),
        }
    }
    let (daily, ..) = expected.unwrap();
    assert_eq!(daily.daily.len(), 7);
    assert_eq!(daily.total.settlements, 40);
    // 0.00 + 0.01 + ... + 0.39.
    assert_eq!(daily.total.spend, money("7.80"));
}
