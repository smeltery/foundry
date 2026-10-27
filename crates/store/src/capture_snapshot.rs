use crate::{
    AttemptFence, CheckpointCut, CheckpointRef, CoverageManifest, Result, Store, ToolOutcome,
    capture_bounds, checkpoint_cut, checkpoint_read, conflict,
};
use sha2::{Digest, Sha256};

/// Includes one original prompt; excessive history rejects the whole snapshot.
pub const MAX_CAPTURE_EVIDENCE_ENTRIES: usize = 1_024;

/// Durable facts only: no native inclusion, delivery, or configuration authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureSnapshot {
    pub head: Option<CheckpointRef>,
    pub cut: CheckpointCut,
    pub coverage: CoverageManifest,
    pub prompt_digest: [u8; 32],
    pub prompt_bytes: u64,
    pub messages: Vec<CaptureMessageEvidence>,
    pub tools: Vec<CaptureToolEvidence>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureMessageEvidence {
    pub id: String,
    pub sequence: u64,
    pub attempt_id: String,
    pub text_digest: [u8; 32],
    pub text_bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureToolEvidence {
    pub call_id: String,
    pub attempt_id: String,
    pub tool: String,
    pub input_digest: [u8; 32],
    pub outcome: ToolOutcome,
    pub output_digest: Option<[u8; 32]>,
}

impl Store {
    /// One fenced read snapshot, with complete sorted coverage and bounded digest evidence.
    /// No raw contents/tokens are returned. Bounds do not cover SQLite cache or scan time.
    pub fn capture_snapshot(&self, fence: &AttemptFence) -> Result<CaptureSnapshot> {
        let tx = self.connection.unchecked_transaction()?;
        checkpoint_cut::owner(&tx, fence)?;
        let snapshot = snapshot(&tx, &fence.session_id)?;
        checkpoint_cut::owner(&tx, fence)?;
        tx.commit()?;
        Ok(snapshot)
    }
}

pub(crate) fn snapshot(
    connection: &rusqlite::Connection,
    session_id: &str,
) -> Result<CaptureSnapshot> {
    let (message_count, tool_count) = capture_bounds::preflight(connection, session_id)?;
    let cut = checkpoint_cut::durable_cut(connection, session_id)?;
    let head = checkpoint_read::head(connection, session_id)?
        .map(|row| row.private().map(|private| private.reference))
        .transpose()?;
    let (prompt_digest, prompt_bytes) =
        checkpoint_cut::with_prompt(connection, session_id, |text| {
            (
                <[u8; 32]>::from(Sha256::digest(text.as_bytes())),
                text.len() as u64,
            )
        })?;
    let mut messages = Vec::with_capacity(message_count);
    let mut query = connection.prepare(
            "SELECT id,sequence,attempt_id,text FROM messages WHERE session_id=?1 AND state='delivered' ORDER BY id COLLATE BINARY",
        )?;
    let mut rows = query.query([session_id])?;
    while let Some(row) = rows.next()? {
        let text = checkpoint_cut::text(row, 3)?;
        messages.push(CaptureMessageEvidence {
            id: checkpoint_cut::text(row, 0)?.into(),
            sequence: row.get::<_, i64>(1)? as u64,
            attempt_id: checkpoint_cut::text(row, 2)?.into(),
            text_digest: Sha256::digest(text.as_bytes()).into(),
            text_bytes: text.len() as u64,
        });
    }
    drop(rows);
    drop(query);
    let mut tools = Vec::with_capacity(tool_count);
    let mut query = connection.prepare(
            "SELECT call_id,attempt_id,tool,input_digest,outcome,output_digest FROM tool_calls WHERE session_id=?1 ORDER BY call_id COLLATE BINARY",
        )?;
    let mut rows = query.query([session_id])?;
    while let Some(row) = rows.next()? {
        let outcome = match checkpoint_cut::text(row, 4)? {
            "success" => ToolOutcome::Success,
            "failed" => ToolOutcome::Failed,
            _ => return Err(conflict("unresolved capture tool")),
        };
        let output_digest: Option<[u8; 32]> = row.get(5)?;
        if outcome == ToolOutcome::Success && output_digest.is_none() {
            return Err(conflict("successful capture tool has no result digest"));
        }
        tools.push(CaptureToolEvidence {
            call_id: checkpoint_cut::text(row, 0)?.into(),
            attempt_id: checkpoint_cut::text(row, 1)?.into(),
            tool: checkpoint_cut::text(row, 2)?.into(),
            input_digest: row.get(3)?,
            outcome,
            output_digest,
        });
    }
    drop(rows);
    drop(query);
    let coverage = CoverageManifest {
        message_ids: messages.iter().map(|m| m.id.clone()).collect(),
        tool_call_ids: tools.iter().map(|t| t.call_id.clone()).collect(),
    };
    Ok(CaptureSnapshot {
        head,
        cut,
        coverage,
        prompt_digest,
        prompt_bytes,
        messages,
        tools,
    })
}
