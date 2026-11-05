//! Usage metering: one token and cost record per attempt, and read-only reports
//! over them (daily consumption, totals grouped by session, user or account,
//! and metric time series). Records live in the budget database, added by its
//! own schema step 2, so they stay outside the main store's migration chain.
//!
//! Reports use UTC calendar days, as the provider specification requires; a
//! window includes its start day and excludes its end day.
//!
//! Known limitations:
//! - Recording is a separate transaction from settling the attempt's
//!   reservation. It is idempotent by `attempt_id`, so the caller settles, then
//!   records, and retries either step safely; a crash between the two leaves
//!   the spend committed but unreported until the record is retried.
//! - Hosted quota (`consumption.quota`) and purchased credits are held on
//!   D46-5 and D46-6. Per-account totals are the local part of a quota report.
//! - Wire names are provisional pending NIC-34 D1/D6.
pub mod calendar;
mod database;
pub mod record;
pub mod report;

pub use calendar::{UtcDate, Window};
pub use record::{AttemptUsage, Recording};
pub use report::{
    Bucket, DailyEntry, DailyReport, GroupBy, GroupKey, GroupTotal, GroupedReport, MetricsPoint,
    Totals,
};

pub(crate) use database::SCHEMA_STEP;

use crate::amount::Currency;
use crate::clock::Clock;
use crate::database::{self as budget_database, Storage};
use crate::error::BudgetError;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::path::Path;
use std::sync::{Arc, Mutex};

struct Shared {
    clock: Arc<dyn Clock>,
    connection: Mutex<Connection>,
}

/// Records and reports attempt usage. It opens the same file as
/// [`crate::BudgetStore`] with its own connection; cloning shares it.
#[derive(Clone)]
pub struct UsageStore {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for UsageStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("UsageStore").finish_non_exhaustive()
    }
}

impl UsageStore {
    /// Opens or creates the budget database at `path`, upgrading an older
    /// budget schema in place.
    pub fn open(path: impl AsRef<Path>, clock: Arc<dyn Clock>) -> Result<Self, BudgetError> {
        Ok(Self::with_connection(
            budget_database::open(path.as_ref())?,
            clock,
        ))
    }

    /// A private in-memory database; nothing outlives the store.
    pub fn open_in_memory(clock: Arc<dyn Clock>) -> Result<Self, BudgetError> {
        Ok(Self::with_connection(
            budget_database::open_in_memory()?,
            clock,
        ))
    }

    fn with_connection(connection: Connection, clock: Arc<dyn Clock>) -> Self {
        Self {
            shared: Arc::new(Shared {
                clock,
                connection: Mutex::new(connection),
            }),
        }
    }

    /// Records a settled attempt at the store clock's current time. Recording
    /// the same attempt again with identical content returns
    /// [`Recording::AlreadyRecorded`] and counts nothing; different content
    /// under the same `attempt_id` is `conflict`.
    pub fn record(&self, attempt: &AttemptUsage) -> Result<Recording, BudgetError> {
        attempt.validate()?;
        self.transaction(TransactionBehavior::Immediate, |transaction, now_ms| {
            database::insert(transaction, attempt, now_ms)
        })
    }

    /// `consumption.daily`. `None` selects the last 30 UTC days including
    /// today. Every attempt in the window must be priced in `currency`.
    pub fn daily(
        &self,
        window: Option<Window>,
        currency: Currency,
    ) -> Result<DailyReport, BudgetError> {
        self.read(window, |window, records| {
            report::daily(window, currency, records)
        })
    }

    /// `consumption.daily` grouped by session, user or account: whole-window
    /// totals, highest spend first.
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
        self.transaction(TransactionBehavior::Deferred, |transaction, now_ms| {
            let window = window.map_or_else(|| Window::default_at(now_ms), Ok)?;
            aggregate(window, &database::in_window(transaction, window)?)
        })
    }

    /// Runs `operation` in one transaction, reading the clock after `BEGIN`,
    /// and commits only on success.
    fn transaction<T>(
        &self,
        behavior: TransactionBehavior,
        operation: impl FnOnce(&Transaction<'_>, u64) -> Result<T, BudgetError>,
    ) -> Result<T, BudgetError> {
        let mut connection =
            self.shared
                .connection
                .lock()
                .map_err(|_| BudgetError::Unavailable {
                    detail: "connection lock poisoned".into(),
                })?;
        let transaction = connection.transaction_with_behavior(behavior).storage()?;
        let now_ms = self.shared.clock.now_ms();
        let value = operation(&transaction, now_ms)?;
        transaction.commit().storage()?;
        Ok(value)
    }
}
