use crate::{
    ContinueInput, ContinueMessageBinding, ContinueMessageInput, Result, checkpoint_codec,
    checkpoint_cut, conflict, restore_read,
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

pub(crate) fn request_digest(
    row: &restore_read::Row,
    input: &ContinueInput,
    message: Option<&ContinueMessageBinding>,
) -> [u8; 32] {
    let Some(message) = message else {
        return restore_read::continue_digest(row, input);
    };
    checkpoint_cut::digest(
        b"foundry.restore.continue.v2",
        &[
            &restore_read::continue_digest(row, input),
            message.message_id.as_bytes(),
            &message.sequence.to_be_bytes(),
            &message.text_digest,
            &message.text_bytes.to_be_bytes(),
        ],
    )
}

pub(crate) fn load(
    connection: &Connection,
    row: &restore_read::Row,
) -> Result<Option<ContinueMessageBinding>> {
    let valid: Option<bool> = connection
        .query_row(
            "SELECT typeof(session_id)='text' AND session_id=?2
         AND typeof(message_id)='text' AND length(CAST(message_id AS BLOB)) BETWEEN 1 AND 128
         AND typeof(sequence)='integer' AND sequence>0
         AND typeof(text_digest)='blob' AND length(text_digest)=32
         AND typeof(text_bytes)='integer' AND text_bytes BETWEEN 1 AND 262144
         FROM context_continue_messages WHERE restore_id=?1",
            params![row.id, row.session],
            |r| r.get(0),
        )
        .optional()?;
    match valid {
        None => return Ok(None),
        Some(true) => {}
        Some(false) => return Err(conflict("invalid bound message receipt")),
    }
    let message = connection.query_row(
        "SELECT message_id,sequence,text_digest,text_bytes FROM context_continue_messages WHERE restore_id=?1",
        [&row.id], |r| Ok(ContinueMessageBinding {
            message_id:r.get(0)?,sequence:r.get(1)?,text_digest:r.get(2)?,text_bytes:r.get(3)?,
        }),
    )?;
    checkpoint_codec::identifier(&message.message_id)?;
    Ok(Some(message))
}

pub(crate) fn select(
    connection: &Connection,
    session: &str,
    input: &ContinueMessageInput,
) -> Result<ContinueMessageBinding> {
    // Preflight before decoding any owned text/identity. Legacy ordinary queue IDs remain valid.
    let valid: Option<bool> = connection
        .query_row(
            "SELECT typeof(id)='text' AND length(CAST(id AS BLOB)) BETWEEN 1 AND 128
         AND typeof(text)='text' AND length(CAST(text AS BLOB)) BETWEEN 1 AND 262144
         AND typeof(sequence)='integer' AND sequence>0
         FROM messages WHERE session_id=?1 AND state='pending' ORDER BY sequence LIMIT 1",
            [session],
            |r| r.get(0),
        )
        .optional()?;
    if valid != Some(true) {
        return Err(conflict(
            "oldest pending message missing or exceeds context bounds",
        ));
    }
    let message = connection.query_row(
        "SELECT id,sequence,text FROM messages WHERE session_id=?1 AND state='pending' ORDER BY sequence LIMIT 1",
        [session], |r| {
            let text = r.get_ref(2)?.as_str()?;
            Ok(ContinueMessageBinding {
                message_id:r.get(0)?,sequence:r.get(1)?,text_digest:Sha256::digest(text.as_bytes()).into(),
                text_bytes:text.len() as u64,
            })
        },
    )?;
    checkpoint_codec::identifier(&message.message_id)?;
    if message.message_id != input.message_id || message.text_digest != input.expected_text_digest {
        return Err(conflict(
            "Continue must claim the exact oldest pending message",
        ));
    }
    Ok(message)
}

pub(crate) fn insert(
    connection: &Connection,
    row: &restore_read::Row,
    message: &ContinueMessageBinding,
) -> Result<()> {
    connection.execute(
        "INSERT INTO context_continue_messages(restore_id,session_id,message_id,sequence,text_digest,text_bytes)
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![row.id,row.session,message.message_id,message.sequence,message.text_digest.as_slice(),message.text_bytes],
    )?;
    Ok(())
}
