#![allow(dead_code)]
use foundry_budget::{Amount, BudgetStore, CostUnit, Currency, ManualClock};
use std::sync::Arc;

/// The ISO 4217 code reserved for testing; no real currency is ever used.
pub fn xts() -> CostUnit {
    CostUnit::Money(Currency::parse("XTS").unwrap())
}

pub fn money(text: &str) -> Amount {
    Amount::parse(xts(), text).unwrap()
}

pub fn tokens(count: u64) -> Amount {
    Amount::from_atoms(CostUnit::OutputTokens, u128::from(count)).unwrap()
}

pub fn manual_store() -> (BudgetStore, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::default());
    (BudgetStore::new(clock.clone()), clock)
}

/// SplitMix64: a small deterministic generator, so every property case can be
/// replayed from its printed seed without an extra dependency.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ (mixed >> 31)
    }

    /// Uniform in `0..bound`; `bound` must be nonzero.
    pub fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}
