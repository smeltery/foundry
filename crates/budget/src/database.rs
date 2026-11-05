//! The crate's own SQLite file. It is separate from the main store and its
//! migration chain, and carries its own schema version table and
//! `application_id`, so it never adopts another program's database.
//!
//! The file is created with the process umask's default permissions; callers
//! that need tighter access must place it in a directory they control.
//!
//! Amounts are stored as decimal TEXT because money atoms are 128-bit and SQLite
//! integers are 64-bit.
use crate::amount::{CostUnit, Currency};
use crate::error::BudgetError;
use crate::ledger::Ledger;
use crate::store::{BudgetId, ReservationId};
use rusqlite::{Connection, ErrorCode, OptionalExtension, TransactionBehavior, params};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

const SCHEMA_VERSION: i64 = 1;

/// Marks a file as a budget database in the SQLite header ("FBDG").
const APPLICATION_ID: i64 = 0x4642_4447;

/// How long a writer waits for another connection's transaction to finish.
const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

const SCHEMA: &str = "
CREATE TABLE budget_schema_version (version INTEGER NOT NULL);
CREATE TABLE budgets (
    id INTEGER PRIMARY KEY,
    parent INTEGER REFERENCES budgets(id),
    unit TEXT NOT NULL,
    currency TEXT,
    ceiling TEXT,
    committed TEXT NOT NULL,
    reserved TEXT NOT NULL,
    overrun INTEGER NOT NULL,
    admitted_at_ms INTEGER NOT NULL,
    wall_clock_ms INTEGER
);
CREATE TABLE reservations (
    id INTEGER PRIMARY KEY,
    budget INTEGER NOT NULL REFERENCES budgets(id),
    atoms TEXT NOT NULL
);
";

pub(crate) trait Storage<T> {
    fn storage(self) -> Result<T, BudgetError>;
}

impl<T> Storage<T> for rusqlite::Result<T> {
    fn storage(self) -> Result<T, BudgetError> {
        self.map_err(|error| unavailable(error.to_string()))
    }
}

fn unavailable(detail: impl Into<String>) -> BudgetError {
    BudgetError::Unavailable {
        detail: detail.into(),
    }
}

pub(crate) fn open(path: &Path) -> Result<Connection, BudgetError> {
    let connection = Connection::open(path).storage()?;
    connection.busy_timeout(BUSY_TIMEOUT).storage()?;
    enable_wal(&connection)?;
    prepare(connection)
}

/// An in-memory database has no journal file, so it skips WAL.
pub(crate) fn open_in_memory() -> Result<Connection, BudgetError> {
    let connection = Connection::open_in_memory().storage()?;
    connection.busy_timeout(BUSY_TIMEOUT).storage()?;
    prepare(connection)
}

/// Switching a fresh file to WAL needs an exclusive lock and bypasses the busy
/// handler, so concurrent openers retry here for up to the busy timeout. The
/// mode SQLite reports back must be `wal`; anything else is refused.
fn enable_wal(connection: &Connection) -> Result<(), BudgetError> {
    let started = Instant::now();
    loop {
        let mode =
            connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0));
        match mode {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
            Ok(mode) => return Err(unavailable(format!("journal mode {mode} instead of wal"))),
            Err(error) if is_busy(&error) && started.elapsed() < BUSY_TIMEOUT => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(unavailable(error.to_string())),
        }
    }
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

/// Creates the schema in an empty file, or checks that an existing file is a
/// budget database at the supported version. A file holding anything else is
/// refused untouched.
fn prepare(mut connection: Connection) -> Result<Connection, BudgetError> {
    connection
        .execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")
        .storage()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .storage()?;
    let application_id: i64 = transaction
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .storage()?;
    let objects: i64 = transaction
        .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
        .storage()?;
    match (application_id, objects) {
        (0, 0) => {
            transaction.execute_batch(SCHEMA).storage()?;
            transaction
                .execute(
                    "INSERT INTO budget_schema_version (version) VALUES (?1)",
                    [SCHEMA_VERSION],
                )
                .storage()?;
            transaction
                .execute_batch(&format!("PRAGMA application_id = {APPLICATION_ID};"))
                .storage()?;
        }
        (APPLICATION_ID, _) => {
            let version: Option<i64> = transaction
                .query_row("SELECT version FROM budget_schema_version", [], |row| {
                    row.get(0)
                })
                .optional()
                .storage()?;
            if version != Some(SCHEMA_VERSION) {
                let found = version.map_or("none".to_owned(), |version| version.to_string());
                return Err(unavailable(format!("unsupported schema version {found}")));
            }
        }
        _ => return Err(unavailable("not a budget database")),
    }
    transaction.commit().storage()?;
    Ok(connection)
}

fn to_sql(value: u64) -> Result<i64, BudgetError> {
    i64::try_from(value).map_err(|_| BudgetError::invalid("milliseconds"))
}

fn from_sql(value: i64) -> Result<u64, BudgetError> {
    u64::try_from(value).map_err(|_| unavailable("negative stored value"))
}

fn atoms(text: &str) -> Result<u128, BudgetError> {
    text.parse()
        .map_err(|_| unavailable("malformed stored amount"))
}

fn unit_columns(unit: CostUnit) -> (&'static str, Option<String>) {
    match unit {
        CostUnit::Money(currency) => (unit.name(), Some(currency.code().to_owned())),
        _ => (unit.name(), None),
    }
}

fn unit_from(name: &str, currency: Option<&str>) -> Result<CostUnit, BudgetError> {
    let corrupt = || unavailable("malformed stored unit");
    match (name, currency) {
        ("input_tokens", None) => Ok(CostUnit::InputTokens),
        ("output_tokens", None) => Ok(CostUnit::OutputTokens),
        ("compute_ms", None) => Ok(CostUnit::ComputeMs),
        ("money", Some(code)) => Ok(CostUnit::Money(
            Currency::parse(code).map_err(|_| corrupt())?,
        )),
        _ => Err(corrupt()),
    }
}

pub(crate) fn insert_budget(
    connection: &Connection,
    ledger: &Ledger,
) -> Result<BudgetId, BudgetError> {
    let (unit, currency) = unit_columns(ledger.unit);
    let wall_clock_ms = ledger.wall_clock_ms.map(to_sql).transpose()?;
    connection
        .execute(
            "INSERT INTO budgets (parent, unit, currency, ceiling, committed, reserved, overrun,
                                  admitted_at_ms, wall_clock_ms)
             VALUES (?1, ?2, ?3, ?4, '0', '0', 0, ?5, ?6)",
            params![
                ledger.parent.map(BudgetId::get_sql),
                unit,
                currency,
                ledger.ceiling.map(|ceiling| ceiling.to_string()),
                to_sql(ledger.admitted_at_ms)?,
                wall_clock_ms,
            ],
        )
        .storage()?;
    Ok(BudgetId::from_sql(connection.last_insert_rowid()))
}

fn load(connection: &Connection, id: BudgetId) -> Result<Ledger, BudgetError> {
    let row = connection
        .query_row(
            "SELECT parent, unit, currency, ceiling, committed, reserved, overrun,
                    admitted_at_ms, wall_clock_ms
             FROM budgets WHERE id = ?1",
            [id.get_sql()],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                ))
            },
        )
        .optional()
        .storage()?
        .ok_or(BudgetError::NotFound { what: "budget" })?;
    let (parent, unit, currency, ceiling, committed, reserved, overrun, admitted, wall_clock) = row;
    Ok(Ledger {
        id,
        parent: parent.map(BudgetId::from_sql),
        unit: unit_from(&unit, currency.as_deref())?,
        ceiling: ceiling.as_deref().map(atoms).transpose()?,
        committed: atoms(&committed)?,
        reserved: atoms(&reserved)?,
        overrun,
        admitted_at_ms: from_sql(admitted)?,
        wall_clock_ms: wall_clock.map(from_sql).transpose()?,
    })
}

/// The budget followed by each ancestor up to the root.
pub(crate) fn chain(connection: &Connection, id: BudgetId) -> Result<Vec<Ledger>, BudgetError> {
    let mut chain = vec![load(connection, id)?];
    while let Some(parent) = chain.last().and_then(|ledger| ledger.parent) {
        chain.push(load(connection, parent)?);
    }
    Ok(chain)
}

pub(crate) fn save(connection: &Connection, chain: &[Ledger]) -> Result<(), BudgetError> {
    let mut statement = connection
        .prepare_cached(
            "UPDATE budgets SET committed = ?1, reserved = ?2, overrun = ?3 WHERE id = ?4",
        )
        .storage()?;
    for ledger in chain {
        statement
            .execute(params![
                ledger.committed.to_string(),
                ledger.reserved.to_string(),
                ledger.overrun,
                ledger.id.get_sql(),
            ])
            .storage()?;
    }
    Ok(())
}

pub(crate) fn insert_reservation(
    connection: &Connection,
    budget: BudgetId,
    reserved_atoms: u128,
) -> Result<ReservationId, BudgetError> {
    connection
        .execute(
            "INSERT INTO reservations (budget, atoms) VALUES (?1, ?2)",
            params![budget.get_sql(), reserved_atoms.to_string()],
        )
        .storage()?;
    Ok(ReservationId::from_sql(connection.last_insert_rowid()))
}

/// Removes an open reservation and returns its budget and reserved atoms.
pub(crate) fn take_reservation(
    connection: &Connection,
    id: ReservationId,
) -> Result<(BudgetId, u128), BudgetError> {
    let row = connection
        .query_row(
            "DELETE FROM reservations WHERE id = ?1 RETURNING budget, atoms",
            [id.get_sql()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .storage()?
        .ok_or(BudgetError::NotFound {
            what: "reservation",
        })?;
    Ok((BudgetId::from_sql(row.0), atoms(&row.1)?))
}
