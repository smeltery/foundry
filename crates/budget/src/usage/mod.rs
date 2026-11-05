//! Usage metering: one token and cost row per metered settlement, and
//! read-only reports over them (daily consumption, totals grouped by session,
//! user, account or attempt, and metric time series).
//!
//! A reservation made with [`crate::BudgetStore::reserve_attempt`] persists its
//! [`Attempt`] beside the reservation row until it settles, so even an orphaned
//! reservation still names its attempt. Settling it, by `commit_usage`,
//! release or drop, writes the usage row in
//! the same `BEGIN IMMEDIATE` transaction that commits the ledger, with the
//! committed amount as the cost and the settlement time as the day. Reports
//! therefore always agree with `committed` spend: a dropped reservation is
//! reported at its full charge, a crash leaves neither written, and a retried
//! settlement finds its reservation gone and writes nothing.
//!
//! Rows live in the budget database, added by its own schema step 2, so they
//! stay outside the main store's migration chain. Reports use UTC calendar
//! days, as the provider specification requires; a window includes its start
//! day and excludes its end day.
//!
//! Known limitations:
//! - A dropped reservation's token counts are unknown and recorded as zero.
//! - Hosted quota (`consumption.quota`) and purchased credits are held on
//!   D46-5 and D46-6. Per-account totals are the local part of a quota report.
//! - Wire names are provisional pending NIC-34 D1/D6.
pub mod calendar;
pub(crate) mod database;
pub mod record;
pub mod report;

pub use calendar::{UtcDate, Window};
pub use record::Attempt;
pub use report::{
    Bucket, DailyEntry, DailyReport, GroupBy, GroupKey, GroupTotal, GroupedReport, MetricsPoint,
    Totals,
};

pub(crate) use database::SCHEMA_STEP;

use crate::amount::Currency;
use crate::error::BudgetError;
use crate::store::BudgetStore;
use rusqlite::TransactionBehavior;

/// Read-only consumption reports over one budget store's usage rows, from
/// [`BudgetStore::usage`]. Reading changes nothing.
#[derive(Clone, Debug)]
pub struct UsageReports {
    store: BudgetStore,
}

impl UsageReports {
    pub(crate) fn new(store: BudgetStore) -> Self {
        Self { store }
    }

    /// `consumption.daily` in `currency`. `None` selects the last 30 UTC days
    /// including today.
    pub fn daily(
        &self,
        window: Option<Window>,
        currency: Currency,
    ) -> Result<DailyReport, BudgetError> {
        self.read(window, |window, records| {
            report::daily(window, currency, records)
        })
    }

    /// `consumption.daily` grouped by session, user, account or attempt:
    /// whole-window totals in `currency`, highest spend first.
    pub fn grouped(
        &self,
        window: Option<Window>,
        currency: Currency,
        group_by: GroupBy,
    ) -> Result<GroupedReport, BudgetError> {
        self.read(window, |window, records| {
            report::grouped(window, currency, group_by, records)
        })
    }

    /// `consumption.metrics`: token and compute totals per bucket with
    /// activity, oldest first.
    pub fn metrics(
        &self,
        window: Option<Window>,
        bucket: Bucket,
    ) -> Result<Vec<MetricsPoint>, BudgetError> {
        self.read(window, |_, records| report::metrics(bucket, records))
    }

    fn read<T>(
        &self,
        window: Option<Window>,
        aggregate: impl FnOnce(Window, &[report::Stored]) -> Result<T, BudgetError>,
    ) -> Result<T, BudgetError> {
        self.store
            .transaction(TransactionBehavior::Deferred, |transaction, now_ms| {
                let window = window.map_or_else(|| Window::default_at(now_ms), Ok)?;
                aggregate(window, &database::in_window(transaction, window)?)
            })
    }
}
