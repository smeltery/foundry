//! Fixture attempts, a settable clock and window helpers shared by the usage
//! tests. All prices are the synthetic fixture table's, in `XTS`.
#![allow(dead_code)]
use foundry_budget::usage::{AttemptUsage, Recording, UsageStore, UtcDate, Window};
use foundry_budget::{Amount, Clock, CostUnit, Currency, Usage};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub const HOUR_MS: u64 = 3_600_000;
pub const DAY_MS: u64 = 86_400_000;

pub fn currency() -> Currency {
    Currency::parse("XTS").unwrap()
}

pub fn at(date: &str) -> u64 {
    UtcDate::parse(date).unwrap().start_ms()
}

pub fn window(start: &str, end: &str) -> Window {
    Window::new(UtcDate::parse(start).unwrap(), UtcDate::parse(end).unwrap()).unwrap()
}

/// A clock set to absolute fixture times, in any order.
#[derive(Default)]
pub struct FixtureClock(AtomicU64);

impl FixtureClock {
    pub fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for FixtureClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

pub fn attempt(id: &str, cost: &str, compute_ms: u64) -> AttemptUsage {
    AttemptUsage {
        attempt_id: id.into(),
        provider_id: "fixture".into(),
        account_key: "primary".into(),
        model: "fixture-small".into(),
        session_id: None,
        user_id: None,
        usage: Usage {
            input_tokens: 100,
            output_tokens: 10,
            compute_ms,
        },
        cost: money(cost),
    }
}

pub fn usage_store() -> (UsageStore, Arc<FixtureClock>) {
    let clock = Arc::new(FixtureClock::default());
    (UsageStore::open_in_memory(clock.clone()).unwrap(), clock)
}

/// Records each `(time, attempt)` at its time.
pub fn record_all(store: &UsageStore, clock: &FixtureClock, attempts: &[(u64, AttemptUsage)]) {
    for (time, attempt) in attempts {
        clock.set(*time);
        assert_eq!(store.record(attempt).unwrap(), Recording::Recorded);
    }
}

/// Money in `XTS`.
pub fn money(text: &str) -> Amount {
    Amount::parse(xts(), text).unwrap()
}

pub fn xts() -> CostUnit {
    CostUnit::Money(currency())
}
