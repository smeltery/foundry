//! The owning handle for one open reservation.
use crate::amount::Amount;
use crate::error::BudgetError;
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
    open: bool,
}

impl Reservation {
    pub(crate) fn new(
        store: BudgetStore,
        id: ReservationId,
        budget: BudgetId,
        amount: Amount,
    ) -> Self {
        Self {
            store,
            id,
            budget,
            amount,
            open: true,
        }
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
    pub fn commit(&mut self, actual: Amount) -> Result<Settlement, BudgetError> {
        if !self.open {
            return Err(BudgetError::Conflict {
                what: "reservation",
            });
        }
        let settlement = self.store.settle(self.id, actual)?;
        self.open = false;
        Ok(settlement)
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
            let _ = self.store.settle(self.id, self.amount);
        }
    }
}
