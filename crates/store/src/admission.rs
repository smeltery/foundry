use crate::{AttemptFence, Result, StoreError, conflict, now_ms, read_session, save_session};
use foundry_contracts::{Lifecycle, Session};
use rusqlite::{Connection, Transaction, params};
use std::time::Duration;
use uuid::Uuid;

pub(crate) fn deadline(duration: Duration) -> Result<i64> {
    now_ms()?
        .checked_add(duration.as_millis() as i64)
        .ok_or(StoreError::Clock)
}

/// Both owner types acquire exclusion in their enclosing IMMEDIATE transaction.
pub(crate) fn exclusive(
    connection: &Connection,
    session: &str,
    restore: Option<&str>,
) -> Result<()> {
    let occupied: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts WHERE session_id=?1 AND active=1)
         OR EXISTS(SELECT 1 FROM context_restores WHERE session_id=?1
             AND phase IN ('loading','restored') AND (?2 IS NULL OR id!=?2))",
        params![session, restore],
        |row| row.get(0),
    )?;
    if occupied {
        return Err(conflict(
            "session already has an execution or restore owner",
        ));
    }
    Ok(())
}

pub(crate) fn admit(
    tx: &Transaction<'_>,
    session_id: &str,
    expected_version: u64,
    duration: Duration,
    restore: Option<&str>,
) -> Result<(AttemptFence, Session)> {
    exclusive(tx, session_id, restore)?;
    let mut session = read_session(tx, session_id)?;
    if session.version != expected_version || session.active_attempt_id.is_some() {
        return Err(conflict("session changed or already has an active attempt"));
    }
    crate::work::advance(tx, session_id)?;
    let fence = AttemptFence {
        session_id: session_id.into(),
        attempt_id: Uuid::new_v4().to_string(),
        token: Uuid::new_v4().to_string(),
    };
    tx.execute(
        "INSERT INTO attempts(id,session_id,token,expires_ms,active) VALUES(?1,?2,?3,?4,1)",
        params![
            fence.attempt_id,
            session_id,
            fence.token,
            deadline(duration)?
        ],
    )?;
    session.active_attempt_id = Some(fence.attempt_id.clone());
    session.lifecycle = Lifecycle::Running;
    session.outcome = None;
    session.result = None;
    save_session(tx, &mut session)?;
    Ok((fence, session))
}
