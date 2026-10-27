use crate::{Result, Store, conflict, read_session};
use rusqlite::{OptionalExtension, Transaction, params};

impl Store {
    /// Monotonic admission/evidence revision, independent of session version and events.
    /// Migrated sessions start at zero; this counter does not reconstruct old execution.
    pub fn work_revision(&self, session_id: &str) -> Result<u64> {
        read_session(&self.connection, session_id)?;
        let revision: Option<i64> = self
            .connection
            .query_row(
                "SELECT revision FROM session_work WHERE session_id=?1",
                [session_id],
                |row| row.get(0),
            )
            .optional()?;
        let revision = revision.ok_or_else(|| conflict("session work revision is missing"))?;
        u64::try_from(revision).map_err(|_| conflict("session work revision is invalid"))
    }
}

/// Only callable with the transaction performing the associated mutation.
/// The predicate prevents SQLite from promoting an overflowing addition to REAL.
pub(crate) fn advance(tx: &Transaction<'_>, session_id: &str) -> Result<()> {
    let changed = tx.execute(
        "UPDATE session_work SET revision=revision+1 WHERE session_id=?1 AND revision<?2",
        params![session_id, i64::MAX],
    )?;
    if changed != 1 {
        return Err(conflict("session work revision is missing or exhausted"));
    }
    Ok(())
}
