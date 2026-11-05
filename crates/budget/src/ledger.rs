//! Accounting over one delegation chain: the budget first, then each ancestor up
//! to the root. These functions validate before they change anything, so a
//! refused operation leaves every ledger untouched.
use crate::amount::{Amount, CostUnit};
use crate::error::{BudgetError, ExhaustedCause};
use crate::store::{BudgetId, DeadlineState};

pub(crate) struct Ledger {
    pub id: BudgetId,
    pub parent: Option<BudgetId>,
    pub unit: CostUnit,
    pub ceiling: Option<u128>,
    pub committed: u128,
    pub reserved: u128,
    pub overrun: bool,
    /// Absolute admission time in clock milliseconds; with `wall_clock_ms` it
    /// fixes the deadline independently of any process restart.
    pub admitted_at_ms: u64,
    pub wall_clock_ms: Option<u64>,
}

impl Ledger {
    fn deadline_at_ms(&self) -> Option<u64> {
        self.wall_clock_ms
            .map(|ms| self.admitted_at_ms.saturating_add(ms))
    }

    fn remaining(&self) -> Option<u128> {
        self.ceiling
            .map(|ceiling| ceiling.saturating_sub(self.committed.saturating_add(self.reserved)))
    }
}

/// The most a new reservation could take across the chain; `None` when no
/// member has a ceiling.
pub(crate) fn remaining(chain: &[Ledger]) -> Option<u128> {
    chain.iter().filter_map(Ledger::remaining).min()
}

/// The earliest deadline in the chain. A deadline is exceeded from the exact
/// millisecond it falls on: `wall_clock_ms` is the last moment not allowed.
pub(crate) fn deadline(chain: &[Ledger], now_ms: u64) -> DeadlineState {
    match chain.iter().filter_map(Ledger::deadline_at_ms).min() {
        None => DeadlineState::Unbounded,
        Some(at) if now_ms >= at => DeadlineState::Exceeded,
        Some(at) => DeadlineState::Remaining { ms: at - now_ms },
    }
}

pub(crate) fn dispatch_allowed(chain: &[Ledger], now_ms: u64) -> bool {
    !chain.iter().any(|ledger| ledger.overrun) && deadline(chain, now_ms) != DeadlineState::Exceeded
}

/// Reserves `amount` on every chain member, or refuses with the nearest member
/// whose deadline, overrun or ceiling stops it.
pub(crate) fn reserve(
    chain: &mut [Ledger],
    amount: Amount,
    now_ms: u64,
) -> Result<(), BudgetError> {
    for ledger in chain.iter() {
        let refuse = |cause| {
            Err(BudgetError::BudgetExhausted {
                budget: ledger.id,
                cause,
            })
        };
        if ledger.deadline_at_ms().is_some_and(|at| now_ms >= at) {
            return refuse(ExhaustedCause::DeadlineExceeded);
        }
        if ledger.overrun {
            return refuse(ExhaustedCause::Overrun);
        }
        let total = ledger
            .committed
            .checked_add(ledger.reserved)
            .and_then(|open| open.checked_add(amount.atoms()))
            .ok_or(BudgetError::invalid("amount"))?;
        Amount::from_atoms(amount.unit(), total)?;
        if ledger.ceiling.is_some_and(|ceiling| total > ceiling) {
            return refuse(ExhaustedCause::Ceiling);
        }
    }
    for ledger in chain.iter_mut() {
        ledger.reserved += amount.atoms();
    }
    Ok(())
}

/// Moves `reserved_atoms` out of every chain member's open reservations and
/// commits `actual` in their place. An actual cost above the reservation is
/// still committed in full and marks the whole chain overrun: children share
/// their ancestors' budget, so an overrun anywhere stops dispatch for every
/// budget that shares it. Returns whether this settlement overran.
pub(crate) fn settle(
    chain: &mut [Ledger],
    reserved_atoms: u128,
    actual: Amount,
) -> Result<bool, BudgetError> {
    for ledger in chain.iter() {
        let total = ledger
            .committed
            .checked_add(actual.atoms())
            .ok_or(BudgetError::invalid("amount"))?;
        Amount::from_atoms(actual.unit(), total)?;
    }
    let overrun = actual.atoms() > reserved_atoms;
    for ledger in chain.iter_mut() {
        ledger.reserved = ledger.reserved.saturating_sub(reserved_atoms);
        ledger.committed += actual.atoms();
        ledger.overrun |= overrun;
    }
    Ok(overrun)
}
