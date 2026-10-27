use crate::{Result, Store, conflict, now_ms, read_session, save_session};
use foundry_contracts::{CancelMode, ContractError, Lifecycle, Outcome, Session};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

/// Treat the token as an ownership credential; do not expose it in event output.
#[derive(Clone, PartialEq, Eq)]
pub struct AttemptFence {
    pub session_id: String,
    pub attempt_id: String,
    pub token: String,
}

impl Store {
    pub fn begin_attempt(
        &mut self,
        session_id: &str,
        expected_version: u64,
    ) -> Result<AttemptFence> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (fence, _) =
            crate::admission::admit(&tx, session_id, expected_version, self.lease_duration, None)?;
        tx.commit()?;
        Ok(fence)
    }

    pub fn heartbeat(&mut self, fence: &AttemptFence) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_fence(&tx, fence)?;
        let deadline = crate::admission::deadline(self.lease_duration)?;
        tx.execute(
            "UPDATE attempts SET expires_ms=?1 WHERE id=?2",
            params![deadline, fence.attempt_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn finish_attempt(
        &mut self,
        fence: &AttemptFence,
        lifecycle: Lifecycle,
        outcome: Option<Outcome>,
        result: Option<String>,
    ) -> Result<Session> {
        validate_terminal(lifecycle, outcome)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = finish_in_transaction(&tx, fence, lifecycle, outcome, result, |_, _| Ok(()))?;
        tx.commit()?;
        Ok(session)
    }

    /// Cancellation is a durable request, not evidence that execution has stopped.
    pub fn request_cancel(&mut self, session_id: &str, mode: CancelMode) -> Result<Session> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut session = read_session(&tx, session_id)?;
        if crate::restores::cancel_reserved(&tx, session_id, mode)? {
            tx.commit()?;
            return Ok(session);
        }
        if let Some(id) = &session.active_attempt_id {
            let previous = cancel_mode(&tx, id)?;
            let next = if previous == Some(CancelMode::Hard) {
                CancelMode::Hard
            } else {
                mode
            };
            if previous != Some(next) {
                tx.execute(
                    "UPDATE attempts SET cancel=?1 WHERE id=?2",
                    params![
                        match next {
                            CancelMode::Graceful => "graceful",
                            CancelMode::Hard => "hard",
                        },
                        id
                    ],
                )?;
                save_session(&tx, &mut session)?;
            }
        } else if session.lifecycle == Lifecycle::Queued {
            session.lifecycle = Lifecycle::Suspended;
            save_session(&tx, &mut session)?;
        }
        tx.commit()?;
        Ok(session)
    }

    pub fn cancellation(&self, fence: &AttemptFence) -> Result<Option<CancelMode>> {
        check_fence(&self.connection, fence)?;
        cancel_mode(&self.connection, &fence.attempt_id)
    }

    /// Explicitly reclaim expired leases. No completed tools or outcomes are inferred.
    /// An owner which misses its renewal deadline is stale even if its process is alive.
    pub fn recover_expired(&mut self) -> Result<Vec<Session>> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expired = {
            let mut statement =
                tx.prepare("SELECT id,session_id FROM attempts WHERE active=1 AND expires_ms<=?1")?;
            let rows = statement.query_map([now_ms()?], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut recovered = Vec::new();
        for (attempt_id, session_id) in expired {
            let mut session = read_session(&tx, &session_id)?;
            session.lifecycle = Lifecycle::Suspended;
            session.outcome = None;
            session.result = None;
            session.active_attempt_id = None;
            tx.execute("UPDATE attempts SET active=0 WHERE id=?1", [&attempt_id])?;
            tx.execute("UPDATE context_restores SET phase='expired' WHERE attempt_id=?1 AND phase='continuing'", [&attempt_id])?;
            save_session(&tx, &mut session)?;
            recovered.push(session);
        }
        tx.commit()?;
        Ok(recovered)
    }
}

pub(crate) fn check_fence(connection: &Connection, fence: &AttemptFence) -> Result<()> {
    let valid: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts WHERE id=?1 AND session_id=?2 AND token=?3 AND active=1 AND expires_ms>?4)",
        params![fence.attempt_id, fence.session_id, fence.token, now_ms()?], |row| row.get(0))?;
    if !valid {
        return Err(conflict("attempt lease is expired or fenced out"));
    }
    Ok(())
}
fn cancel_mode(connection: &Connection, attempt: &str) -> Result<Option<CancelMode>> {
    let value: Option<Option<String>> = connection
        .query_row("SELECT cancel FROM attempts WHERE id=?1", [attempt], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(match value.flatten().as_deref() {
        Some("hard") => Some(CancelMode::Hard),
        Some("graceful") => Some(CancelMode::Graceful),
        _ => None,
    })
}

pub(crate) fn validate_terminal(lifecycle: Lifecycle, outcome: Option<Outcome>) -> Result<()> {
    if matches!(lifecycle, Lifecycle::Queued | Lifecycle::Running)
        || (lifecycle == Lifecycle::Exit) != outcome.is_some()
    {
        return Err(ContractError::Invalid(
            "exit requires an outcome; error/suspended must not have one".into(),
        )
        .into());
    }
    Ok(())
}

fn check_terminal(
    connection: &Connection,
    fence: &AttemptFence,
    lifecycle: Lifecycle,
) -> Result<()> {
    check_fence(connection, fence)?;
    if cancel_mode(connection, &fence.attempt_id)?.is_some() && lifecycle != Lifecycle::Suspended {
        return Err(conflict("cancelled attempt must finish suspended"));
    }
    if lifecycle == Lifecycle::Exit {
        let unresolved: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM tool_calls WHERE session_id=?1 AND (outcome IS NULL OR outcome='uncertain'))",
            [&fence.session_id], |row| row.get(0),
        )?;
        if unresolved {
            return Err(conflict(
                "unresolved tool calls require error or suspension",
            ));
        }
    }
    Ok(())
}

pub(crate) fn finish_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    fence: &AttemptFence,
    lifecycle: Lifecycle,
    outcome: Option<Outcome>,
    result: Option<String>,
    before_deactivate: impl FnOnce(&rusqlite::Transaction<'_>, &Session) -> Result<()>,
) -> Result<Session> {
    validate_terminal(lifecycle, outcome)?;
    check_terminal(tx, fence, lifecycle)?;
    let mut session = read_session(tx, &fence.session_id)?;
    session.lifecycle = lifecycle;
    session.outcome = outcome;
    session.result = result;
    session.active_attempt_id = None;
    save_session(tx, &mut session)?;
    before_deactivate(tx, &session)?;
    tx.execute(
        "UPDATE context_restores SET phase='finished' WHERE attempt_id=?1 AND phase='continuing'",
        [&fence.attempt_id],
    )?;
    check_terminal(tx, fence, lifecycle)?;
    let changed = tx.execute(
        "UPDATE attempts SET active=0 WHERE id=?1 AND session_id=?2 AND token=?3
         AND active=1 AND expires_ms>?4",
        params![fence.attempt_id, fence.session_id, fence.token, now_ms()?],
    )?;
    if changed != 1 {
        return Err(conflict("attempt expired before finalization"));
    }
    Ok(session)
}
