//! The owning handle for one open reservation.
use crate::amount::Amount;
use crate::error::BudgetError;
use crate::price::Usage;
use crate::store::{BudgetId, BudgetStore, ReservationId};
use crate::usage::Attempt;

/// The result of settling a reservation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settlement {
    /// The actual cost committed for this call.
    pub charged: Amount,
    /// The unused part of the reservation returned to the budget.
    pub released: Amount,
    /// The budget's total committed spend after this settlement.
    pub committed: Amount,
    /// The actual cost exceeded the reservation.
    pub overrun: bool,
    /// Whether the chain still admits new dispatches (no overrun, deadline open).
    pub dispatch_allowed: bool,
}

/// An open reservation. A cancelled or failed call settles what it consumed with
/// [`Reservation::commit`], or [`Reservation::release`] when it consumed nothing.
///
/// Dropping an unsettled handle, for example when the task holding it is
/// cancelled or panics, charges the full reservation: the call may have run, and
/// its cost is unknown, so the conservative choice is the maximum it was allowed.
/// That settlement can block for up to the store's busy timeout, and if it fails
/// the reservation stays counted with no reconcile API yet.
#[must_use = "an unsettled reservation charges its full amount when dropped"]
#[derive(Debug)]
pub struct Reservation {
    store: BudgetStore,
    id: ReservationId,
    budget: BudgetId,
    amount: Amount,
    /// Set for a metered reservation; every settlement then writes usage.
    attempt: Option<Attempt>,
    open: bool,
}

impl Reservation {
    pub(crate) fn new(
        store: BudgetStore,
        id: ReservationId,
        budget: BudgetId,
        amount: Amount,
        attempt: Option<Attempt>,
    ) -> Self {
        Self {
            store,
            id,
            budget,
            amount,
            attempt,
            open: true,
        }
    }

    fn settle(&mut self, actual: Amount, usage: Usage) -> Result<Settlement, BudgetError> {
        if !self.open {
            return Err(BudgetError::Conflict {
                what: "reservation",
            });
        }
        let metered = self.attempt.as_ref().map(|attempt| (attempt, usage));
        let settlement = self.store.settle(self.id, actual, metered)?;
        self.open = false;
        Ok(settlement)
    }

    pub fn id(&self) -> ReservationId {
        self.id
    }

    pub fn budget(&self) -> BudgetId {
        self.budget
    }

    pub fn amount(&self) -> Amount {
        self.amount
    }

    /// Commits the actual cost and releases the remainder. On error the
    /// reservation stays open, so the caller may retry with a corrected amount.
    /// A metered reservation records zero token counts this way; use
    /// [`Reservation::commit_usage`] to record them.
    pub fn commit(&mut self, actual: Amount) -> Result<Settlement, BudgetError> {
        self.settle(actual, Usage::default())
    }

    /// Commits like [`Reservation::commit`] and records the call's token and
    /// compute counts in the usage row. Only a metered reservation, made with
    /// [`BudgetStore::reserve_attempt`], has a row to record them in.
    pub fn commit_usage(
        &mut self,
        actual: Amount,
        usage: Usage,
    ) -> Result<Settlement, BudgetError> {
        if self.attempt.is_none() {
            return Err(BudgetError::invalid("usage"));
        }
        self.settle(actual, usage)
    }

    /// Releases the whole reservation for a call that consumed nothing.
    pub fn release(&mut self) -> Result<Settlement, BudgetError> {
        self.commit(Amount::zero(self.amount.unit()))
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.open {
            // Drop cannot report an error. If the store is unavailable the
            // reservation stays open and counted, which refuses spend rather
            // than allowing it.
            // A metered drop records the full charge with unknown (zero) counts.
            let metered = self
                .attempt
                .as_ref()
                .map(|attempt| (attempt, Usage::default()));
            let _ = self.store.settle(self.id, self.amount, metered);
        }
    }
}
