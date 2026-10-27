use crate::{
    AttemptFence, ContinueAcceptanceInput, ContinueAcceptanceRef, Result, Store, checkpoint_cut,
    conflict, continue_codec, continue_read, restore_read,
};
use rusqlite::{TransactionBehavior, params};

impl Store {
    /// Atomically record a preauthorized native receipt observation and deliver its instruction.
    /// Native matching is the trusted caller's obligation, not a claim made by this store.
    pub fn accept_restore_continuation(
        &mut self,
        fence: &AttemptFence,
        input: ContinueAcceptanceInput,
    ) -> Result<ContinueAcceptanceRef> {
        let request = input.request_digest()?;
        let body = continue_codec::encode(&input)?;
        let data = continue_codec::event(&input, request)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        owner(&tx, fence)?;
        if fence.session_id != input.intent.session_id
            || fence.attempt_id != input.authorization.attempt_id
        {
            return Err(conflict("acceptance requires the admitted attempt owner"));
        }
        let restore = restore_read::load(&tx, &input.intent.restore_id)?;
        if restore.phase != "continuing" || restore.cancel.is_some() {
            return Err(conflict("restore is not continuing or is cancelled"));
        }
        if let Some(existing) = continue_read::verified(&tx, &fence.session_id, &restore.id)? {
            if existing.reference.request_digest != request {
                return Err(conflict("acceptance request has different content"));
            }
            owner(&tx, fence)?;
            tx.commit()?;
            return Ok(existing.reference);
        }
        crate::continue_baseline::verify(&tx, &input, &restore)?;
        continue_read::message(&tx, &input, "in_flight")?;
        let event_id = format!("continue-accepted:{}", restore.id);
        let orphan: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE session_id=?1 AND id=?2)",
            params![fence.session_id, event_id],
            |r| r.get(0),
        )?;
        if orphan {
            return Err(conflict(
                "reserved acceptance event already exists without acceptance",
            ));
        }
        let before_revision: i64 = tx.query_row(
            "SELECT revision FROM session_work WHERE session_id=?1",
            [&fence.session_id],
            |r| r.get(0),
        )?;
        let expected_revision = before_revision
            .checked_add(2)
            .filter(|_| {
                before_revision >= 0
                    && before_revision as u64 >= input.authorization.admitted_work_revision
            })
            .ok_or_else(|| conflict("acceptance work revision exhausted"))?;
        let charge = crate::checkpoint_limits::acceptance_charge(&input, body.len(), data.len())?;
        crate::checkpoint_limits::check(&tx, &fence.session_id, charge)?;
        let last: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM events WHERE session_id=?1",
            [&fence.session_id],
            |r| r.get(0),
        )?;
        let sequence = last
            .checked_add(1)
            .filter(|n| *n > 0)
            .ok_or_else(|| conflict("acceptance event sequence exhausted"))?;
        let committed = crate::now_ms()?;
        let record = continue_read::record_digest(request, &restore.id, sequence as u64, committed);
        crate::work::advance(&tx, &fence.session_id)?;
        changed(tx.execute("INSERT INTO events(session_id,sequence,id,attempt_id,revision,kind,data) VALUES(?1,?2,?3,?4,0,'continue_accepted',?5)",
            params![fence.session_id,sequence,event_id,fence.attempt_id,data])?)?;
        crate::work::advance(&tx, &fence.session_id)?;
        changed(tx.execute("UPDATE messages SET state='delivered' WHERE session_id=?1 AND id=?2 AND state='in_flight' AND attempt_id=?3 AND sequence=?4",
            params![fence.session_id,input.intent.message_id,fence.attempt_id,input.intent.message_sequence])?)?;
        changed(tx.execute("INSERT INTO context_continue_acceptances(restore_id,intent_body,request_digest,event_sequence,committed_ms,record_digest) VALUES(?1,?2,?3,?4,?5,?6)",
            params![restore.id,body,request.as_slice(),sequence,committed,record.as_slice()])?)?;
        // Re-read after triggers and conditional writes; no partially associated receipt can escape.
        let result = continue_read::verified(&tx, &fence.session_id, &restore.id)?
            .ok_or_else(|| conflict("acceptance insertion missing"))?;
        let actual_revision: i64 = tx.query_row(
            "SELECT revision FROM session_work WHERE session_id=?1",
            [&fence.session_id],
            |r| r.get(0),
        )?;
        if actual_revision != expected_revision {
            return Err(conflict("acceptance revision regressed"));
        }
        let final_restore = restore_read::load(&tx, &restore.id)?;
        if final_restore.phase != "continuing" || final_restore.cancel.is_some() {
            return Err(conflict("restore ownership changed during acceptance"));
        }
        owner(&tx, fence)?;
        tx.commit()?;
        Ok(result.reference)
    }
    /// Read-only historical integrity recovery. Never reissues execution/delivery authority.
    pub fn continuation_acceptance(
        &self,
        session_id: &str,
        restore_id: &str,
        expected_request_digest: [u8; 32],
    ) -> Result<Option<ContinueAcceptanceRef>> {
        let tx = self.connection.unchecked_transaction()?;
        let result = continue_read::verified(&tx, session_id, restore_id)?;
        if result
            .as_ref()
            .is_some_and(|v| v.reference.request_digest != expected_request_digest)
        {
            return Err(conflict("historical acceptance request differs"));
        }
        tx.commit()?;
        Ok(result.map(|v| v.reference))
    }
}
fn changed(count: usize) -> Result<()> {
    if count != 1 {
        Err(conflict(
            "acceptance atomic write did not affect exactly one row",
        ))
    } else {
        Ok(())
    }
}

fn owner(db: &rusqlite::Connection, fence: &AttemptFence) -> Result<()> {
    checkpoint_cut::owner(db, fence)?;
    if crate::read_session(db, &fence.session_id)?
        .active_attempt_id
        .as_deref()
        != Some(&fence.attempt_id)
    {
        return Err(conflict("acceptance attempt is not the session owner"));
    }
    Ok(())
}
