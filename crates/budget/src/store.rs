//! The budget store: ledgers persisted in the crate's own SQLite file (see
//! [`crate::database`]), or in an in-memory SQLite database through the same
//! code path.
//!
//! Reserve, settle and delegate each run in one `BEGIN IMMEDIATE` transaction
//! that reads the clock, loads the delegation chain, checks it and writes it
//! back. Concurrent reservers are therefore serialized across threads,
//! connections and processes, and a child never spends past any ancestor.
//!
//! Committed spend, open reservations, overruns and deadlines survive a restart.
//! A reservation left open by a crash stays counted against its ceilings, which
//! refuses spend rather than allowing it.
use crate::amount::{Amount, CostUnit};
use crate::clock::{Clock, WallClock};
use crate::database::{self, Storage};
use crate::error::BudgetError;
use crate::ledger::{self, Ledger};
use crate::reservation::{Reservation, Settlement};
use crate::termination::Termination;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BudgetId(u64);

impl BudgetId {
    pub fn get(&self) -> u64 {
        self.0
    }

    // SQLite assigns positive row IDs, so the conversions are lossless.
    pub(crate) fn from_sql(row_id: i64) -> Self {
        Self(row_id as u64)
    }

    pub(crate) fn get_sql(self) -> i64 {
        self.0 as i64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReservationId(u64);

impl ReservationId {
    pub(crate) fn from_sql(row_id: i64) -> Self {
        Self(row_id as u64)
    }

    pub(crate) fn get_sql(self) -> i64 {
        self.0 as i64
    }
}

/// Whether a budget's effective deadline (the earliest in its chain) has passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeadlineState {
    Unbounded,
    Remaining { ms: u64 },
    Exceeded,
}

impl DeadlineState {
    /// The settlement for the running attempt once the deadline has passed.
    pub fn termination(&self) -> Option<Termination> {
        matches!(self, Self::Exceeded).then(Termination::deadline_exceeded)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetSnapshot {
    pub parent: Option<BudgetId>,
    pub ceiling: Option<Amount>,
    pub committed: Amount,
    pub reserved: Amount,
    /// The most a new reservation could take now, across the whole chain;
    /// `None` when no budget in the chain has a ceiling.
    pub remaining: Option<Amount>,
    pub overrun: bool,
    pub deadline: DeadlineState,
}

struct Shared {
    clock: Arc<dyn Clock>,
    connection: Mutex<Connection>,
}

/// Cloning shares one connection. Separate `open` calls on the same file use
/// separate connections and still serialize through SQLite.
#[derive(Clone)]
pub struct BudgetStore {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for BudgetStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BudgetStore")
            .finish_non_exhaustive()
    }
}

impl BudgetStore {
    /// Opens or creates the budget database at `path`.
    pub fn open(path: impl AsRef<Path>, clock: Arc<dyn Clock>) -> Result<Self, BudgetError> {
        Ok(Self::with_connection(database::open(path.as_ref())?, clock))
    }

    /// A private in-memory database; nothing outlives the store.
    pub fn open_in_memory(clock: Arc<dyn Clock>) -> Result<Self, BudgetError> {
        Ok(Self::with_connection(database::open_in_memory()?, clock))
    }

    fn with_connection(connection: Connection, clock: Arc<dyn Clock>) -> Self {
        Self {
            shared: Arc::new(Shared {
                clock,
                connection: Mutex::new(connection),
            }),
        }
    }

    /// Creates a root budget at execution admission. A `None` ceiling sets no
    /// local limit and keeps the account's own limits.
    pub fn create(
        &self,
        unit: CostUnit,
        ceiling: Option<Amount>,
        wall_clock: WallClock,
    ) -> Result<BudgetId, BudgetError> {
        self.write(|transaction, now_ms| {
            insert(transaction, unit, ceiling, None, wall_clock, now_ms)
        })
    }

    /// Creates a child budget for a delegated session or automatic continuation.
    /// The child shares the parent's spend: every child reservation also counts
    /// against each ancestor. A child ceiling may narrow but never widen the
    /// chain, so it must fit within the parent chain's current remainder. The
    /// child's own deadline is checked alongside every ancestor's, so the
    /// effective deadline is the earliest in the chain.
    pub fn delegate(
        &self,
        parent: BudgetId,
        ceiling: Option<Amount>,
        wall_clock: WallClock,
    ) -> Result<BudgetId, BudgetError> {
        self.write(|transaction, now_ms| {
            let chain = database::chain(transaction, parent)?;
            if let (Some(ceiling), Some(remaining)) = (ceiling, ledger::remaining(&chain))
                && ceiling.atoms() > remaining
            {
                return Err(BudgetError::invalid("child_ceiling"));
            }
            insert(
                transaction,
                chain[0].unit,
                ceiling,
                Some(parent),
                wall_clock,
                now_ms,
            )
        })
    }

    /// Atomically reserves a call's maximum cost before dispatch. It succeeds only
    /// when committed spend plus open reservations plus `amount` stays within the
    /// ceiling of the budget and of every ancestor, no member has overrun and no
    /// deadline in the chain has passed.
    pub fn reserve(&self, budget: BudgetId, amount: Amount) -> Result<Reservation, BudgetError> {
        let id = self.write(|transaction, now_ms| {
            let mut chain = database::chain(transaction, budget)?;
            if chain[0].unit != amount.unit() {
                return Err(BudgetError::invalid("unit"));
            }
            ledger::reserve(&mut chain, amount, now_ms)?;
            database::save(transaction, &chain)?;
            database::insert_reservation(transaction, budget, amount.atoms())
        })?;
        Ok(Reservation::new(self.clone(), id, budget, amount))
    }

    pub fn snapshot(&self, budget: BudgetId) -> Result<BudgetSnapshot, BudgetError> {
        self.transaction(TransactionBehavior::Deferred, |transaction, now_ms| {
            let chain = database::chain(transaction, budget)?;
            let own = &chain[0];
            let amount = |atoms| Amount::from_atoms(own.unit, atoms);
            Ok(BudgetSnapshot {
                parent: own.parent,
                ceiling: own.ceiling.map(amount).transpose()?,
                committed: amount(own.committed)?,
                reserved: amount(own.reserved)?,
                remaining: ledger::remaining(&chain).map(amount).transpose()?,
                overrun: own.overrun,
                deadline: ledger::deadline(&chain, now_ms),
            })
        })
    }

    /// Commits `actual` for the reservation and releases the rest along the chain.
    pub(crate) fn settle(
        &self,
        id: ReservationId,
        actual: Amount,
    ) -> Result<Settlement, BudgetError> {
        self.write(|transaction, now_ms| {
            let (budget, reserved_atoms) = database::take_reservation(transaction, id)?;
            let mut chain = database::chain(transaction, budget)?;
            if chain[0].unit != actual.unit() {
                return Err(BudgetError::invalid("unit"));
            }
            let overrun = ledger::settle(&mut chain, reserved_atoms, actual)?;
            database::save(transaction, &chain)?;
            let unit = actual.unit();
            Ok(Settlement {
                charged: actual,
                released: Amount::from_atoms(unit, reserved_atoms.saturating_sub(actual.atoms()))?,
                committed: Amount::from_atoms(unit, chain[0].committed)?,
                overrun,
                dispatch_allowed: ledger::dispatch_allowed(&chain, now_ms),
            })
        })
    }

    fn write<T>(
        &self,
        operation: impl FnOnce(&Transaction<'_>, u64) -> Result<T, BudgetError>,
    ) -> Result<T, BudgetError> {
        self.transaction(TransactionBehavior::Immediate, operation)
    }

    /// Runs `operation` in one transaction and commits it only on success. The
    /// clock is read after the transaction starts, so the time an operation
    /// checks is never older than the state it sees.
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

fn insert(
    connection: &Connection,
    unit: CostUnit,
    ceiling: Option<Amount>,
    parent: Option<BudgetId>,
    wall_clock: WallClock,
    now_ms: u64,
) -> Result<BudgetId, BudgetError> {
    if ceiling.is_some_and(|ceiling| ceiling.unit() != unit) {
        return Err(BudgetError::invalid("unit"));
    }
    let ledger = Ledger {
        // SQLite assigns the real ID on insert.
        id: BudgetId(0),
        parent,
        unit,
        ceiling: ceiling.map(|ceiling| ceiling.atoms()),
        committed: 0,
        reserved: 0,
        overrun: false,
        admitted_at_ms: now_ms,
        wall_clock_ms: wall_clock.ms(),
    };
    database::insert_budget(connection, &ledger)
}
