//! Budget and accounting core: exact cost units, atomic reservations with
//! commit and release, delegated child budgets capped by their parent, wall-clock
//! deadlines, review rounds, and the settlement values for deadline, quota and
//! review ends. Ledgers persist in the crate's own SQLite file (see [`store`]),
//! outside the main store's migration chain. Per-attempt usage and consumption
//! reports live in [`usage`]. Prices come from a fixture table. All wire names are provisional pending NIC-34 D1/D6.
pub mod amount;
pub mod clock;
mod database;
pub mod error;
mod ledger;
pub mod price;
pub mod reservation;
pub mod review;
pub mod store;
pub mod termination;
pub mod usage;

pub use amount::{Amount, CostUnit, Currency};
pub use clock::{Clock, ManualClock, SystemClock, WallClock};
pub use error::{BudgetError, ExhaustedCause};
pub use price::{ModelPrice, PriceTable, Usage};
pub use reservation::{Reservation, Settlement};
pub use review::{ReviewPolicy, ReviewRounds, ReviewStep};
pub use store::{BudgetId, BudgetSnapshot, BudgetStore, DeadlineState, ReservationId};
pub use termination::{Cause, Lifecycle, Outcome, QuotaCause, Termination};
