//! Usage rows in the budget database. The table arrives through schema step 2 of
//! the budget database's own version table (see [`crate::database`]).
//!
//! One row is written per metered settlement, inside the settlement's own
//! transaction. The settlement consumes its reservation row in that same
//! transaction, so a retried or repeated settlement can never write a second
//! row: it finds no reservation and fails before anything is written.
//!
//! Counts and atoms are stored as decimal TEXT: token counts are unsigned
//! 64-bit and money atoms 128-bit, while SQLite integers are signed 64-bit.
use crate::amount::Amount;
use crate::database::{Storage, unit_columns, unit_from};
use crate::error::BudgetError;
use crate::price::Usage;
use crate::usage::calendar::Window;
use crate::usage::record::Attempt;
use crate::usage::report::Stored;
use rusqlite::{Connection, Row, params};

/// Schema step 2: one usage row per metered settlement.
pub(crate) const SCHEMA_STEP: &str = "
CREATE TABLE usage_records (
    id INTEGER PRIMARY KEY,
    attempt_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    account_key TEXT NOT NULL,
    model TEXT NOT NULL,
    session_id TEXT,
    user_id TEXT,
    input_tokens TEXT NOT NULL,
    output_tokens TEXT NOT NULL,
    compute_ms TEXT NOT NULL,
    unit TEXT NOT NULL,
    currency TEXT,
    cost TEXT NOT NULL,
    settled_at_ms INTEGER NOT NULL
);
CREATE INDEX usage_records_by_time ON usage_records (settled_at_ms, id);
";

const COLUMNS: &str = "attempt_id, provider_id, account_key, model, session_id, user_id,
                       input_tokens, output_tokens, compute_ms, unit, currency, cost,
                       settled_at_ms";

fn corrupt() -> BudgetError {
    BudgetError::Unavailable {
        detail: "malformed stored usage".into(),
    }
}

fn to_sql(ms: u64) -> Result<i64, BudgetError> {
    i64::try_from(ms).map_err(|_| BudgetError::invalid("milliseconds"))
}

/// Writes the row for one settlement: `cost` is the amount the ledger just
/// committed and `settled_at_ms` the settlement transaction's clock reading.
pub(crate) fn insert(
    connection: &Connection,
    attempt: &Attempt,
    usage: Usage,
    cost: Amount,
    settled_at_ms: u64,
) -> Result<(), BudgetError> {
    let (unit, currency) = unit_columns(cost.unit());
    connection
        .prepare_cached(&format!(
            "INSERT INTO usage_records ({COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"
        ))
        .storage()?
        .execute(params![
            attempt.attempt_id,
            attempt.provider_id,
            attempt.account_key,
            attempt.model,
            attempt.session_id,
            attempt.user_id,
            usage.input_tokens.to_string(),
            usage.output_tokens.to_string(),
            usage.compute_ms.to_string(),
            unit,
            currency,
            cost.atoms().to_string(),
            to_sql(settled_at_ms)?,
        ])
        .storage()?;
    Ok(())
}

/// Every row settled inside `window`, ordered by time and then insertion.
pub(crate) fn in_window(
    connection: &Connection,
    window: Window,
) -> Result<Vec<Stored>, BudgetError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {COLUMNS} FROM usage_records
             WHERE settled_at_ms >= ?1 AND settled_at_ms < ?2
             ORDER BY settled_at_ms, id"
        ))
        .storage()?;
    let rows = statement
        .query_map(
            params![
                to_sql(window.start().start_ms())?,
                to_sql(window.end().start_ms())?
            ],
            |row| Ok(read(row)),
        )
        .storage()?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row.storage()??);
    }
    Ok(records)
}

/// Decodes one row. SQLite errors surface through the outer `Result`; values
/// this version cannot parse surface through the inner one.
fn read(row: &Row<'_>) -> Result<Stored, BudgetError> {
    let text = |index| row.get::<_, String>(index).storage();
    let count = |index| text(index)?.parse::<u64>().map_err(|_| corrupt());
    let currency: Option<String> = row.get(10).storage()?;
    let unit = unit_from(&text(9)?, currency.as_deref())?;
    let atoms = text(11)?.parse::<u128>().map_err(|_| corrupt())?;
    let settled_at_ms: i64 = row.get(12).storage()?;
    Ok(Stored {
        attempt: Attempt {
            attempt_id: text(0)?,
            provider_id: text(1)?,
            account_key: text(2)?,
            model: text(3)?,
            session_id: row.get(4).storage()?,
            user_id: row.get(5).storage()?,
        },
        usage: Usage {
            input_tokens: count(6)?,
            output_tokens: count(7)?,
            compute_ms: count(8)?,
        },
        cost: Amount::from_atoms(unit, atoms).map_err(|_| corrupt())?,
        settled_at_ms: u64::try_from(settled_at_ms).map_err(|_| corrupt())?,
    })
}
