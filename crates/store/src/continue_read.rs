use crate::{
    ContinueAcceptanceInput, ContinueAcceptanceRef, Result, conflict, continue_codec, restore_read,
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

/// Internal verified immutable evidence. No credential/permit and no witness-fact recursion.
pub(crate) struct VerifiedAcceptance {
    pub input: ContinueAcceptanceInput,
    pub reference: ContinueAcceptanceRef,
}
pub(crate) fn record_digest(
    request: [u8; 32],
    restore: &str,
    sequence: u64,
    committed: i64,
) -> [u8; 32] {
    crate::checkpoint_cut::digest(
        b"foundry.restore.acceptance.record.v1",
        &[
            &request,
            restore.as_bytes(),
            &sequence.to_be_bytes(),
            &committed.to_be_bytes(),
        ],
    )
}
pub(crate) fn message(db: &Connection, input: &ContinueAcceptanceInput, state: &str) -> Result<()> {
    let i = &input.intent;
    let valid:bool = db.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE session_id=?1 AND id=?2
        AND state=?3 AND attempt_id=?4 AND typeof(sequence)='integer' AND sequence=?5
        AND typeof(text)='text' AND length(CAST(text AS BLOB))=?6 AND length(CAST(text AS BLOB)) BETWEEN 1 AND 262144)",
        params![i.session_id,i.message_id,state,input.authorization.attempt_id,i.message_sequence,i.message_text_bytes],|r|r.get(0))?;
    if !valid {
        return Err(conflict(
            "acceptance message identity or delivery state mismatch",
        ));
    }
    let hash: [u8; 32] = db.query_row(
        "SELECT text FROM messages WHERE session_id=?1 AND id=?2",
        params![i.session_id, i.message_id],
        |r| Ok(Sha256::digest(r.get_ref(0)?.as_str()?.as_bytes()).into()),
    )?;
    if hash != i.message_text_digest {
        return Err(conflict("acceptance message text mismatch"));
    }
    Ok(())
}
pub(crate) fn verified(
    db: &Connection,
    session: &str,
    restore_id: &str,
) -> Result<Option<VerifiedAcceptance>> {
    crate::continue_intent::uuid(session)?;
    crate::continue_intent::uuid(restore_id)?;
    let valid: Option<bool> = db
        .query_row(
            "SELECT typeof(intent_body)='blob' AND length(intent_body) BETWEEN 1 AND 4096
        AND typeof(request_digest)='blob' AND length(request_digest)=32
        AND typeof(record_digest)='blob' AND length(record_digest)=32
        AND typeof(event_sequence)='integer' AND event_sequence>0
        AND typeof(committed_ms)='integer' AND committed_ms>=0
        FROM context_continue_acceptances WHERE restore_id=?1",
            [restore_id],
            |r| r.get(0),
        )
        .optional()?;
    match valid {
        None => return Ok(None),
        Some(true) => {}
        _ => return Err(conflict("invalid stored acceptance envelope")),
    }
    let restore = restore_read::load(db, restore_id)?;
    if restore.session != session {
        return Err(conflict("acceptance belongs to another session"));
    }
    let authorization = restore
        .receipt()?
        .ok_or_else(|| conflict("acceptance authorization missing"))?;
    let (body,request,sequence,committed,record):(Vec<u8>,[u8;32],u64,i64,[u8;32]) = db.query_row(
        "SELECT intent_body,request_digest,event_sequence,committed_ms,record_digest FROM context_continue_acceptances WHERE restore_id=?1",
        [restore_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    let input = continue_codec::decode(&body, authorization)?;
    if input.intent.restore_id != restore_id
        || input.intent.session_id != session
        || input.request_digest()? != request
        || record_digest(request, restore_id, sequence, committed) != record
    {
        return Err(conflict("acceptance record integrity mismatch"));
    }
    crate::continue_baseline::verify(db, &input, &restore)?;
    message(db, &input, "delivered")?;
    let event_id = format!("continue-accepted:{restore_id}");
    let count: i64 = db.query_row(
        "SELECT COUNT(*) FROM events WHERE session_id=?1 AND id=?2",
        params![session, event_id],
        |r| r.get(0),
    )?;
    let event_data = continue_codec::event(&input, request)?;
    let matches:bool = db.query_row("SELECT EXISTS(SELECT 1 FROM events WHERE session_id=?1 AND id=?2 AND sequence=?3
        AND typeof(sequence)='integer' AND typeof(id)='text' AND typeof(session_id)='text'
        AND typeof(attempt_id)='text' AND attempt_id=?4 AND typeof(revision)='integer' AND revision=0 AND kind='continue_accepted' AND typeof(data)='text' AND data=?5)",
        params![session,event_id,sequence,input.authorization.attempt_id,event_data],|r|r.get(0))?;
    let duplicates:i64 = db.query_row("SELECT COUNT(*) FROM context_continue_acceptances a JOIN context_restores r ON r.id=a.restore_id
        WHERE r.session_id=?1 AND a.event_sequence=?2",params![session,sequence],|r|r.get(0))?;
    let attempt: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts WHERE id=?1 AND session_id=?2)",
        params![input.authorization.attempt_id, session],
        |r| r.get(0),
    )?;
    if count != 1 || !matches || duplicates != 1 || !attempt {
        return Err(conflict("acceptance event association mismatch"));
    }
    Ok(Some(VerifiedAcceptance {
        reference: ContinueAcceptanceRef {
            session_id: session.into(),
            restore_id: restore_id.into(),
            attempt_id: input.authorization.attempt_id.clone(),
            message_id: input.intent.message_id.clone(),
            request_digest: request,
            record_digest: record,
            event_id,
            event_sequence: sequence,
            committed_ms: committed,
        },
        input,
    }))
}
