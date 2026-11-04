use crate::{Result, Store, conflict, now_ms, read_session, save_session};
use foundry_contracts::{CancelMode, CancelReceipt, ContractError, Lifecycle, Outcome, Session};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

/// Treat the token as an ownership credential; do not expose it in event output.
#[derive(Clone, PartialEq, Eq)]
pub struct AttemptFence {
    pub session_id: String,
    pub attempt_id: String,
    pub token: String,
}

/// Session state after a cancel call, with the receipt reported to callers.
#[derive(Debug, Clone, PartialEq)]
pub struct CancelRecord {
    pub session: Session,
    pub receipt: CancelReceipt,
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
        Ok(self.cancel_session(session_id, mode)?.session)
    }

    /// Record a cancel and describe its effect. A live attempt or restore owner
    /// gets an upgrade-only request and keeps its lifecycle until it settles; a
    /// queued session suspends; a suspended session becomes terminally
    /// `cancelled`. Settled and cancelled sessions are left unchanged.
    pub fn cancel_session(&mut self, session_id: &str, mode: CancelMode) -> Result<CancelRecord> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut session = read_session(&tx, session_id)?;
        let (requested, effective) = if let Some(change) =
            crate::restores::cancel_reserved(&tx, session_id, mode)?
        {
            change
        } else if let Some(id) = &session.active_attempt_id {
            let previous = cancel_mode(&tx, id)?;
            let next = upgraded(previous, mode);
            let requested = previous != Some(next);
            if requested {
                tx.execute(
                    "UPDATE attempts SET cancel=?1 WHERE id=?2",
                    params![cancel_text(next), id],
                )?;
                save_session(&tx, &mut session)?;
            }
            (requested, next)
        } else {
            let next = match session.lifecycle {
                Lifecycle::Queued => Some(Lifecycle::Suspended),
                Lifecycle::Suspended => Some(Lifecycle::Cancelled),
                Lifecycle::Running | Lifecycle::Exit | Lifecycle::Error | Lifecycle::Cancelled => {
                    None
                }
            };
            if let Some(lifecycle) = next {
                session.lifecycle = lifecycle;
                save_session(&tx, &mut session)?;
            }
            (next.is_some(), mode)
        };
        tx.commit()?;
        let receipt = CancelReceipt {
            ok: true,
            requested,
            mode: effective,
            status: session.lifecycle,
        };
        Ok(CancelRecord { session, receipt })
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
    Ok(parse_cancel(value.flatten().as_deref()))
}

pub(crate) fn parse_cancel(value: Option<&str>) -> Option<CancelMode> {
    match value {
        Some("hard") => Some(CancelMode::Hard),
        Some("graceful") => Some(CancelMode::Graceful),
        _ => None,
    }
}

pub(crate) fn cancel_text(mode: CancelMode) -> &'static str {
    match mode {
        CancelMode::Graceful => "graceful",
        CancelMode::Hard => "hard",
    }
}

/// Hard is sticky: a later graceful request never downgrades it.
pub(crate) fn upgraded(previous: Option<CancelMode>, mode: CancelMode) -> CancelMode {
    if previous == Some(CancelMode::Hard) {
        CancelMode::Hard
    } else {
        mode
    }
}

/// A cancelled session is terminal: no attempt, restore or follow-up may start.
pub(crate) fn refuse_cancelled(session: &Session) -> Result<()> {
    if session.lifecycle == Lifecycle::Cancelled {
        return Err(ContractError::Cancelled(
            "a cancelled session cannot be resumed or continued".into(),
        )
        .into());
    }
    Ok(())
}

pub(crate) fn validate_terminal(lifecycle: Lifecycle, outcome: Option<Outcome>) -> Result<()> {
    // `cancelled` is reached only through the cancel path, never by settling.
    if matches!(
        lifecycle,
        Lifecycle::Queued | Lifecycle::Running | Lifecycle::Cancelled
    ) || (lifecycle == Lifecycle::Exit) != outcome.is_some()
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
