use crate::{AttemptFence, Result, Store, attempts::check_fence, conflict, read_session};
use foundry_contracts::ContractError;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOutcome {
    Success,
    /// The caller has evidence that the failed operation produced no effects.
    Failed,
    /// Effects may have occurred; further dispatch requires reconciliation.
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRecord {
    pub session_id: String,
    pub call_id: String,
    pub attempt_id: String,
    pub tool: String,
    pub input_digest: [u8; 32],
    /// None means claimed, with no durable completion evidence. Never retry it.
    pub outcome: Option<ToolOutcome>,
    pub output_digest: Option<[u8; 32]>,
}

impl Store {
    /// Commit before starting a helper. A repeated call ID is always rejected,
    /// even when the input matches; callers must not interpret it as a new permit.
    pub fn claim_tool(
        &mut self,
        fence: &AttemptFence,
        call_id: &str,
        tool: &str,
        input_digest: [u8; 32],
    ) -> Result<ToolRecord> {
        identifier(call_id, 128)?;
        identifier(tool, 64)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_fence(&tx, fence)?;
        require_not_cancelled(&tx, fence)?;
        if find(&tx, &fence.session_id, call_id)?.is_some() {
            return Err(conflict("tool call ID was already claimed"));
        }
        let unresolved: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tool_calls WHERE session_id=?1 AND (outcome IS NULL OR outcome='uncertain'))",
            [&fence.session_id], |row| row.get(0),
        )?;
        if unresolved {
            return Err(conflict("unresolved tool call prevents dispatch"));
        }
        crate::work::advance(&tx, &fence.session_id)?;
        tx.execute(
            "INSERT INTO tool_calls(session_id,call_id,attempt_id,tool,input_digest) VALUES(?1,?2,?3,?4,?5)",
            params![fence.session_id, call_id, fence.attempt_id, tool, input_digest.as_slice()],
        )?;
        let record = required(&tx, &fence.session_id, call_id)?;
        tx.commit()?;
        Ok(record)
    }

    /// Record observed completion before delivering its result to the adapter.
    /// This transaction cannot make a filesystem mutation atomic with SQLite.
    pub fn finish_tool(
        &mut self,
        fence: &AttemptFence,
        call_id: &str,
        outcome: ToolOutcome,
        output_digest: Option<[u8; 32]>,
    ) -> Result<ToolRecord> {
        if outcome == ToolOutcome::Success && output_digest.is_none() {
            return Err(
                ContractError::Invalid("successful tool requires an output digest".into()).into(),
            );
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_fence(&tx, fence)?;
        require_not_cancelled(&tx, fence)?;
        let record = required(&tx, &fence.session_id, call_id)?;
        if record.attempt_id != fence.attempt_id {
            return Err(conflict("tool call belongs to another attempt"));
        }
        if let Some(existing) = record.outcome {
            if existing == outcome && record.output_digest == output_digest {
                return Ok(record);
            }
            return Err(conflict("tool completion cannot be changed"));
        }
        let name = match outcome {
            ToolOutcome::Success => "success",
            ToolOutcome::Failed => "failed",
            ToolOutcome::Uncertain => "uncertain",
        };
        crate::work::advance(&tx, &fence.session_id)?;
        tx.execute(
            "UPDATE tool_calls SET outcome=?3,output_digest=?4 WHERE session_id=?1 AND call_id=?2",
            params![
                fence.session_id,
                call_id,
                name,
                output_digest.as_ref().map(|value| value.as_slice())
            ],
        )?;
        let record = required(&tx, &fence.session_id, call_id)?;
        tx.commit()?;
        Ok(record)
    }

    pub fn tool_call(&self, session_id: &str, call_id: &str) -> Result<Option<ToolRecord>> {
        read_session(&self.connection, session_id)?;
        find(&self.connection, session_id, call_id)
    }
}

fn identifier(value: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(ContractError::Invalid("invalid tool identifier".into()).into());
    }
    Ok(())
}

fn require_not_cancelled(connection: &Connection, fence: &AttemptFence) -> Result<()> {
    let cancelled: bool = connection.query_row(
        "SELECT cancel IS NOT NULL FROM attempts WHERE id=?1",
        [&fence.attempt_id],
        |row| row.get(0),
    )?;
    if cancelled {
        return Err(conflict(
            "cancelled attempt cannot dispatch or complete tools",
        ));
    }
    Ok(())
}

fn required(connection: &Connection, session_id: &str, call_id: &str) -> Result<ToolRecord> {
    find(connection, session_id, call_id)?.ok_or_else(|| conflict("tool call was not claimed"))
}

fn find(connection: &Connection, session_id: &str, call_id: &str) -> Result<Option<ToolRecord>> {
    Ok(connection.query_row(
        "SELECT session_id,call_id,attempt_id,tool,input_digest,outcome,output_digest FROM tool_calls WHERE session_id=?1 AND call_id=?2",
        params![session_id,call_id], |row| {
            let raw: Option<String> = row.get(5)?;
            let outcome = match raw.as_deref() {
                None => None,
                Some("success") => Some(ToolOutcome::Success),
                Some("failed") => Some(ToolOutcome::Failed),
                Some("uncertain") => Some(ToolOutcome::Uncertain),
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(ToolRecord {
                session_id: row.get(0)?, call_id: row.get(1)?, attempt_id: row.get(2)?,
                tool: row.get(3)?, input_digest: row.get(4)?, outcome, output_digest: row.get(6)?,
            })
        },
    ).optional()?)
}
