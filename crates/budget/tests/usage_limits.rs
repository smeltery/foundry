//! Report windows, checked sums, currency mixing, record validation and
//! order independence for usage metering.
mod support;
mod usage_fixtures;

use foundry_budget::usage::{AttemptUsage, Bucket, GroupBy, UtcDate, Window};
use foundry_budget::{Amount, CostUnit, Currency, Usage};
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
    assert_eq!(window("2024-01-01", "2025-01-01").days(), 366);
    assert_eq!(
        Window::last_days(at("2026-01-01"), 367)
            .unwrap_err()
            .class(),
        "invalid_request"
    );

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
fn sums_are_checked_and_never_wrap() {
    let (store, clock) = usage_store();
    let huge = |id: &str| AttemptUsage {
        usage: Usage {
            input_tokens: u64::MAX,
            ..Usage::default()
        },
        ..attempt(id, "0", 0)
    };
    record_all(&store, &clock, &[(1, huge("a")), (2, huge("b"))]);
    let range = Some(window("1970-01-01", "1970-01-02"));
    assert_eq!(
        store.daily(range, currency()).unwrap_err().class(),
        "invalid_request"
    );
    assert_eq!(
        store.metrics(range, Bucket::Day).unwrap_err().class(),
        "invalid_request"
    );

    let (store, clock) = usage_store();
    let max_money = Amount::from_atoms(xts(), u128::MAX).unwrap();
    let rich = |id: &str| AttemptUsage {
        cost: max_money,
        ..attempt(id, "0", 0)
    };
    record_all(&store, &clock, &[(1, rich("a")), (2, rich("b"))]);
    let grouped = store.grouped(range, currency(), GroupBy::Account);
    assert_eq!(grouped.unwrap_err().class(), "invalid_request");
}

#[test]
fn mixed_currencies_are_refused_rather_than_combined() {
    let (store, clock) = usage_store();
    let other = CostUnit::Money(Currency::parse("XXX").unwrap());
    let foreign = AttemptUsage {
        cost: Amount::parse(other, "1").unwrap(),
        ..attempt("b", "0", 0)
    };
    record_all(&store, &clock, &[(1, attempt("a", "1", 0)), (2, foreign)]);
    let range = Some(window("1970-01-01", "1970-01-02"));
    assert_eq!(
        store.daily(range, currency()).unwrap_err().class(),
        "conflict"
    );
    // Metrics carry no spend, so they still count both attempts.
    assert_eq!(store.metrics(range, Bucket::Day).unwrap()[0].attempts, 2);
}

#[test]
fn malformed_records_are_refused_before_storage() {
    let (store, _clock) = usage_store();
    let cases: Vec<(AttemptUsage, &str)> = vec![
        (attempt("", "0", 0), "attempt_id"),
        (attempt(&"x".repeat(257), "0", 0), "attempt_id"),
        (
            AttemptUsage {
                provider_id: "p".repeat(65),
                ..attempt("a", "0", 0)
            },
            "provider_id",
        ),
        (
            AttemptUsage {
                account_key: String::new(),
                ..attempt("a", "0", 0)
            },
            "account_key",
        ),
        (
            AttemptUsage {
                model: "bad\0model".into(),
                ..attempt("a", "0", 0)
            },
            "model",
        ),
        (
            AttemptUsage {
                session_id: Some(String::new()),
                ..attempt("a", "0", 0)
            },
            "session_id",
        ),
        (
            AttemptUsage {
                cost: Amount::from_atoms(CostUnit::InputTokens, 1).unwrap(),
                ..attempt("a", "0", 0)
            },
            "cost",
        ),
    ];
    for (record, field) in cases {
        let error = store.record(&record).unwrap_err();
        assert_eq!(error.to_string(), format!("invalid_request: {field}"));
    }
    let range = Some(window("1970-01-01", "1970-01-02"));
    assert!(store.metrics(range, Bucket::Day).unwrap().is_empty());
}

#[test]
fn reports_do_not_depend_on_recording_order() {
    let base = at("2026-01-01");
    // Each fixture attempt has a fixed time spread over a week.
    let fixtures: Vec<(u64, AttemptUsage)> = (0..40u64)
        .map(|index| {
            let record = AttemptUsage {
                account_key: format!("account-{}", index % 3),
                session_id: (index % 4 != 0).then(|| format!("session-{}", index % 5)),
                ..attempt(
                    &format!("attempt-{index:02}"),
                    &format!("0.{index:02}"),
                    index,
                )
            };
            (base + (index % 7) * DAY_MS + index * HOUR_MS / 2, record)
        })
        .collect();
    let range = Some(window("2026-01-01", "2026-01-09"));
    let mut expected = None;
    for seed in 0..8 {
        let mut order = fixtures.clone();
        let mut rng = Rng::new(seed);
        for index in (1..order.len()).rev() {
            order.swap(index, rng.below(index as u64 + 1) as usize);
        }
        let (store, clock) = usage_store();
        record_all(&store, &clock, &order);
        let reports = (
            store.daily(range, currency()).unwrap(),
            store.grouped(range, currency(), GroupBy::Session).unwrap(),
            store.grouped(range, currency(), GroupBy::Account).unwrap(),
            store.metrics(range, Bucket::Hour).unwrap(),
        );
        match &expected {
            None => expected = Some(reports),
            Some(expected) => assert_eq!(expected, &reports, "seed {seed}"),
        }
    }
    let (daily, ..) = expected.unwrap();
    assert_eq!(daily.daily.len(), 7);
    assert_eq!(daily.total.attempts, 40);
    // 0.00 + 0.01 + ... + 0.39.
    assert_eq!(daily.total.spend, money("7.80"));
}
