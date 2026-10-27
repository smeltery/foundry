use crate::{AttemptFence, Result, Store, admission, checkpoint_cut, conflict};
use foundry_contracts::{MAX_PROMPT_BYTES, Session};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

/// One initial admission, not proof that Start ran or that native context is valid.
/// The retained attempt prevents reissue after a lost response. Extracted ordinary
/// credentials remain copyable; the trusted caller must not launch them twice.
pub struct FreshAttempt {
    fence: AttemptFence,
    session: Session,
}
impl FreshAttempt {
    pub fn fence(&self) -> &AttemptFence {
        &self.fence
    }
    pub fn session(&self) -> &Session {
        &self.session
    }
    pub fn into_parts(self) -> (AttemptFence, Session) {
        (self.fence, self.session)
    }
}

impl Store {
    /// Admit only an initial, queued session with no retained execution history.
    /// Pending and never-claimed withdrawn messages remain untouched. No retry
    /// issues another permit after commit, even if no Start/event is known to exist.
    pub fn begin_fresh_attempt(
        &mut self,
        session_id: &str,
        expected_version: u64,
    ) -> Result<FreshAttempt> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_initial_fresh(&tx, session_id)?;
        let (fence, session) =
            admission::admit(&tx, session_id, expected_version, self.lease_duration, None)?;
        checkpoint_cut::owner(&tx, &fence)?;
        tx.commit()?;
        Ok(FreshAttempt { fence, session })
    }
}

fn require_initial_fresh(tx: &Transaction<'_>, session_id: &str) -> Result<()> {
    // SQLite compares decoded values without copying a second creation document
    // into Rust. This bounds the prompt, not SQLite parsing/cache or scan time.
    let initial: Option<bool> = tx.query_row(
        "SELECT CASE WHEN json_valid(body) AND json_valid(creation) THEN COALESCE(
            json_type(body,'$.id')='text' AND json_extract(body,'$.id')=?1
            AND json_extract(body,'$.lifecycle')='queued'
            AND json_type(body,'$.active_attempt_id')='null'
            AND json_type(body,'$.outcome')='null' AND json_type(body,'$.result')='null'
            AND json_type(body,'$.prompt')='text' AND json_type(creation,'$.prompt')='text'
            AND length(CAST(json_extract(body,'$.prompt') AS BLOB)) BETWEEN 1 AND ?2
            AND CAST(json_extract(body,'$.prompt') AS BLOB)=CAST(json_extract(creation,'$.prompt') AS BLOB)
            AND json_type(body,'$.directory')='text' AND json_type(creation,'$.directory')='text'
            AND CAST(json_extract(body,'$.directory') AS BLOB)=CAST(json_extract(creation,'$.directory') AS BLOB)
            AND json_type(body,'$.model') IN ('null','object')
            AND json_type(creation,'$.model')=json_type(body,'$.model')
            AND json_extract(body,'$.model') IS json_extract(creation,'$.model'),0) ELSE 0 END
         FROM sessions WHERE id=?1", params![session_id,MAX_PROMPT_BYTES], |r|r.get(0),
    ).optional()?;
    if initial != Some(true) {
        return Err(conflict(
            "fresh admission requires an unchanged queued creation",
        ));
    }
    let revision_valid: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM session_work WHERE session_id=?1
            AND typeof(revision)='integer' AND revision>=0)",
        [session_id],
        |r| r.get(0),
    )?;
    if !revision_valid {
        return Err(conflict(
            "fresh admission work revision is missing or invalid",
        ));
    }
    let history: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts WHERE session_id=?1)
         OR EXISTS(SELECT 1 FROM events WHERE session_id=?1)
         OR EXISTS(SELECT 1 FROM tool_calls WHERE session_id=?1)
         OR EXISTS(SELECT 1 FROM context_checkpoints WHERE session_id=?1)
         OR EXISTS(SELECT 1 FROM context_checkpoint_heads WHERE session_id=?1)
         OR EXISTS(SELECT 1 FROM context_witnesses WHERE session_id=?1)
         OR EXISTS(SELECT 1 FROM context_restores WHERE session_id=?1)
         OR EXISTS(SELECT 1 FROM context_continue_messages WHERE session_id=?1)
         OR EXISTS(SELECT 1 FROM context_finalizations f JOIN context_checkpoints c
             ON c.id=f.checkpoint_id WHERE c.session_id=?1)
         OR EXISTS(SELECT 1 FROM messages WHERE session_id=?1 AND
            (typeof(state)!='text' OR state NOT IN ('pending','withdrawn') OR attempt_id IS NOT NULL))",
        [session_id], |r|r.get(0),
    )?;
    if history {
        return Err(conflict(
            "fresh admission rejects retained execution history",
        ));
    }
    Ok(())
}
