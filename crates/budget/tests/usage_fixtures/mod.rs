//! Fixture attempts, a settable clock and window helpers shared by the usage
//! tests. All prices are the synthetic fixture table's, in `XTS`.
#![allow(dead_code)]
use foundry_budget::usage::{Attempt, UtcDate, Window};
use foundry_budget::{
    Amount, BudgetId, BudgetStore, Clock, CostUnit, Currency, Settlement, Usage, WallClock,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub const HOUR_MS: u64 = 3_600_000;
pub const DAY_MS: u64 = 86_400_000;

pub fn currency() -> Currency {
    Currency::parse("XTS").unwrap()
}

pub fn xts() -> CostUnit {
    CostUnit::Money(currency())
}

pub fn money(text: &str) -> Amount {
    Amount::parse(xts(), text).unwrap()
}

pub fn at(date: &str) -> u64 {
    UtcDate::parse(date).unwrap().start_ms()
}

pub fn window(start: &str, end: &str) -> Option<Window> {
    Some(Window::new(UtcDate::parse(start).unwrap(), UtcDate::parse(end).unwrap()).unwrap())
}

/// A clock set to absolute fixture times, in any order. No fixture budget has
/// a deadline, so moving it backwards is harmless.
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

pub fn attempt(id: &str) -> Attempt {
    Attempt {
        attempt_id: id.into(),
        provider_id: "fixture".into(),
        account_key: "primary".into(),
        model: "fixture-small".into(),
        session_id: None,
        user_id: None,
    }
}

pub fn calls(input_tokens: u64, compute_ms: u64) -> Usage {
    Usage {
        input_tokens,
        output_tokens: 10,
        compute_ms,
    }
}

/// An in-memory store with one unbounded `XTS` budget.
pub fn fixture_store() -> (BudgetStore, Arc<FixtureClock>, BudgetId) {
    let clock = Arc::new(FixtureClock::default());
    let store = BudgetStore::open_in_memory(clock.clone()).unwrap();
    let budget = store.create(xts(), None, WallClock::NONE).unwrap();
    (store, clock, budget)
}

/// Reserves `cost` for `attempt` and settles exactly that much at `time`.
pub fn settle(
    store: &BudgetStore,
    clock: &FixtureClock,
    budget: BudgetId,
    time: u64,
    attempt: Attempt,
    cost: Amount,
    usage: Usage,
) -> Settlement {
    clock.set(time);
    store
        .reserve_attempt(budget, cost, attempt)
        .unwrap()
        .commit_usage(cost, usage)
        .unwrap()
}
