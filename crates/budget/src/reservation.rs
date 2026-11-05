//! The owning handle for one open reservation.
use crate::amount::Amount;
use crate::error::BudgetError;
use crate::price::Usage;
use crate::store::{BudgetId, BudgetStore, ReservationId};

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
/// [`Reservation::commit`] (or [`Reservation::commit_usage`] when metered), or
/// [`Reservation::release`] when it consumed nothing.
///
/// Dropping an unsettled handle, for example when the task holding it is
/// cancelled or panics, charges the full reservation: the call may have run, and
/// its cost is unknown, so the conservative choice is the maximum it was allowed.
/// A metered drop also writes the usage row at that full charge, with zero
/// token counts. That settlement can block for up to the store's busy timeout.
/// If it fails, including when only the usage row cannot be written, nothing is
/// settled and the reservation stays counted (fail-closed) with no reconcile
/// API yet.
#[must_use = "an unsettled reservation charges its full amount when dropped"]
#[derive(Debug)]
pub struct Reservation {
    store: BudgetStore,
    id: ReservationId,
    budget: BudgetId,
    amount: Amount,
    /// Made by [`BudgetStore::reserve_attempt`]: its attempt is persisted with
    /// the reservation row, and every settlement writes a usage row.
    metered: bool,
    open: bool,
}

impl Reservation {
    pub(crate) fn new(
        store: BudgetStore,
        id: ReservationId,
        budget: BudgetId,
        amount: Amount,
        metered: bool,
    ) -> Self {
        Self {
            store,
            id,
            budget,
            amount,
            metered,
            open: true,
        }
    }

    fn settle(&mut self, actual: Amount, usage: Usage) -> Result<Settlement, BudgetError> {
        if !self.open {
            return Err(BudgetError::Conflict {
                what: "reservation",
            });
        }
        let settlement = self.store.settle(self.id, actual, usage)?;
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
    /// A retry after a lost acknowledgement returns `conflict`, meaning the
    /// reservation was already settled; it charges and records nothing again.
    ///
    /// A metered reservation must report its token counts through
    /// [`Reservation::commit_usage`], so a nonzero `actual` here is
    /// `invalid_request` rather than a row with silently dropped counts.
    pub fn commit(&mut self, actual: Amount) -> Result<Settlement, BudgetError> {
        if self.metered && actual.atoms() != 0 {
            return Err(BudgetError::invalid("usage"));
        }
        self.settle(actual, Usage::default())
    }

    /// Commits like [`Reservation::commit`] and records the call's token and
    /// compute counts in the usage row, with the same retry behavior. Only a
    /// metered reservation, made with [`BudgetStore::reserve_attempt`], has a
    /// row to record them in.
    pub fn commit_usage(
        &mut self,
        actual: Amount,
        usage: Usage,
    ) -> Result<Settlement, BudgetError> {
        if !self.metered {
            return Err(BudgetError::invalid("usage"));
        }
        self.settle(actual, usage)
    }

    /// Releases the whole reservation for a call that consumed nothing.
    pub fn release(&mut self) -> Result<Settlement, BudgetError> {
        self.settle(Amount::zero(self.amount.unit()), Usage::default())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.open {
            // Drop cannot report an error. If the store is unavailable the
            // reservation stays open and counted, which refuses spend rather
            // than allowing it. A metered drop's token counts are unknown.
            let _ = self.store.settle(self.id, self.amount, Usage::default());
        }
    }
}
