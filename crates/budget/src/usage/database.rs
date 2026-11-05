//! Usage rows in the budget database. The table arrives through schema step 2 of
//! the budget database's own version table (see [`crate::database`]).
//!
//! Counts and atoms are stored as decimal TEXT: token counts are unsigned
//! 64-bit and money atoms 128-bit, while SQLite integers are signed 64-bit.
use crate::amount::{Amount, CostUnit, Currency};
use crate::database::Storage;
use crate::error::BudgetError;
use crate::price::Usage;
use crate::usage::calendar::Window;
use crate::usage::record::{AttemptUsage, Recording};
use crate::usage::report::Stored;
use rusqlite::{Connection, OptionalExtension, Row, params};

/// Schema step 2: per-attempt usage records.
pub(crate) const SCHEMA_STEP: &str = "
CREATE TABLE usage_records (
    attempt_id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL,
    account_key TEXT NOT NULL,
    model TEXT NOT NULL,
    session_id TEXT,
    user_id TEXT,
    input_tokens TEXT NOT NULL,
    output_tokens TEXT NOT NULL,
    compute_ms TEXT NOT NULL,
    currency TEXT NOT NULL,
    cost TEXT NOT NULL,
    recorded_at_ms INTEGER NOT NULL
);
CREATE INDEX usage_records_by_time ON usage_records (recorded_at_ms, attempt_id);
";

const COLUMNS: &str = "attempt_id, provider_id, account_key, model, session_id, user_id,
                       input_tokens, output_tokens, compute_ms, currency, cost, recorded_at_ms";

fn corrupt() -> BudgetError {
    BudgetError::Unavailable {
        detail: "malformed stored usage".into(),
    }
}

fn to_sql(ms: u64) -> Result<i64, BudgetError> {
    i64::try_from(ms).map_err(|_| BudgetError::invalid("recorded_at_ms"))
}

/// Inserts `record`, or reports that the attempt is already recorded. The
/// caller holds an immediate transaction, so the lookup and insert cannot
/// interleave with another writer. A different record under the same attempt
/// is `conflict` and leaves the stored one unchanged.
pub(crate) fn insert(
    connection: &Connection,
    record: &AttemptUsage,
    now_ms: u64,
) -> Result<Recording, BudgetError> {
    let existing = connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM usage_records WHERE attempt_id = ?1"),
            [&record.attempt_id],
            |row| Ok(read(row)),
        )
        .optional()
        .storage()?
        .transpose()?;
    if let Some(existing) = existing {
        if existing.record == *record {
            return Ok(Recording::AlreadyRecorded);
        }
        return Err(BudgetError::Conflict {
            what: "usage_record",
        });
    }
    let CostUnit::Money(currency) = record.cost.unit() else {
        return Err(BudgetError::invalid("cost"));
    };
    connection
        .execute(
            &format!(
                "INSERT INTO usage_records ({COLUMNS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
            ),
            params![
                record.attempt_id,
                record.provider_id,
                record.account_key,
                record.model,
                record.session_id,
                record.user_id,
                record.usage.input_tokens.to_string(),
                record.usage.output_tokens.to_string(),
                record.usage.compute_ms.to_string(),
                currency.code(),
                record.cost.atoms().to_string(),
                to_sql(now_ms)?,
            ],
        )
        .storage()?;
    Ok(Recording::Recorded)
}

/// Every record inside `window`, ordered by time and then attempt.
pub(crate) fn in_window(
    connection: &Connection,
    window: Window,
) -> Result<Vec<Stored>, BudgetError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {COLUMNS} FROM usage_records
             WHERE recorded_at_ms >= ?1 AND recorded_at_ms < ?2
             ORDER BY recorded_at_ms, attempt_id"
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
    let currency = Currency::parse(&text(9)?).map_err(|_| corrupt())?;
    let atoms = text(10)?.parse::<u128>().map_err(|_| corrupt())?;
    let recorded_at_ms: i64 = row.get(11).storage()?;
    Ok(Stored {
        record: AttemptUsage {
            attempt_id: text(0)?,
            provider_id: text(1)?,
            account_key: text(2)?,
            model: text(3)?,
            session_id: row.get(4).storage()?,
            user_id: row.get(5).storage()?,
            usage: Usage {
                input_tokens: count(6)?,
                output_tokens: count(7)?,
                compute_ms: count(8)?,
            },
            cost: Amount::from_atoms(CostUnit::Money(currency), atoms).map_err(|_| corrupt())?,
        },
        recorded_at_ms: u64::try_from(recorded_at_ms).map_err(|_| corrupt())?,
    })
}
