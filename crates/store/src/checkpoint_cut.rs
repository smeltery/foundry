use crate::{
    AttemptFence, CheckpointCut, CoverageManifest, Result, attempts::check_fence, conflict,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

pub(crate) fn field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}
pub(crate) fn digest(domain: &[u8], fields: &[&[u8]]) -> [u8; 32] {
    let mut hash = Sha256::new();
    field(&mut hash, domain);
    for value in fields {
        field(&mut hash, value);
    }
    hash.finalize().into()
}
pub(crate) fn owner(connection: &Connection, fence: &AttemptFence) -> Result<()> {
    check_fence(connection, fence)?;
    let cancelled: bool = connection.query_row(
        "SELECT cancel IS NOT NULL FROM attempts WHERE id=?1",
        [&fence.attempt_id],
        |r| r.get(0),
    )?;
    if cancelled {
        return Err(conflict("cancelled attempt cannot publish checkpoints"));
    }
    Ok(())
}
pub(crate) fn cut(connection: &Connection, fence: &AttemptFence) -> Result<CheckpointCut> {
    owner(connection, fence)?;
    durable_cut(connection, &fence.session_id)
}
pub(crate) fn durable_cut(connection: &Connection, session_id: &str) -> Result<CheckpointCut> {
    let revision: i64 = connection.query_row(
        "SELECT revision FROM session_work WHERE session_id=?1",
        [session_id],
        |r| r.get(0),
    )?;
    let unresolved: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM tool_calls WHERE session_id=?1
             AND (outcome IS NULL OR outcome='uncertain'))
         OR EXISTS(SELECT 1 FROM messages WHERE session_id=?1 AND state='in_flight')",
        [session_id],
        |row| row.get(0),
    )?;
    if unresolved {
        return Err(conflict("unresolved work prevents checkpoint publication"));
    }
    let mut events = Sha256::new();
    field(&mut events, b"foundry.checkpoint.events.v1");
    let mut event_seq = 0;
    let mut statement = connection.prepare(
        "SELECT sequence,id,revision,attempt_id,kind,data FROM events WHERE session_id=?1 ORDER BY sequence",
    )?;
    let mut rows = statement.query([session_id])?;
    while let Some(row) = rows.next()? {
        let seq: i64 = row.get(0)?;
        let rev: i64 = row.get(2)?;
        field(&mut events, &seq.to_be_bytes());
        field(&mut events, text(row, 1)?.as_bytes());
        field(&mut events, &rev.to_be_bytes());
        for index in 3..6 {
            field(&mut events, text(row, index)?.as_bytes());
        }
        event_seq = u64::try_from(seq).map_err(|_| conflict("invalid event sequence"))?;
    }
    let mut inputs = Sha256::new();
    field(&mut inputs, b"foundry.checkpoint.inputs.v1");
    with_prompt(connection, session_id, |prompt| {
        field(&mut inputs, prompt.as_bytes())
    })?;
    let mut statement = connection.prepare(
        "SELECT id,sequence,text,attempt_id FROM messages WHERE session_id=?1 AND state='delivered' ORDER BY sequence",
    )?;
    let mut rows = statement.query([session_id])?;
    while let Some(row) = rows.next()? {
        field(&mut inputs, text(row, 0)?.as_bytes());
        field(&mut inputs, &row.get::<_, i64>(1)?.to_be_bytes());
        field(&mut inputs, text(row, 2)?.as_bytes());
        field(&mut inputs, text(row, 3)?.as_bytes());
    }
    let mut tools = Sha256::new();
    field(&mut tools, b"foundry.checkpoint.tools.v1");
    let mut statement = connection.prepare(
        "SELECT call_id,attempt_id,tool,input_digest,outcome,output_digest FROM tool_calls WHERE session_id=?1 ORDER BY call_id COLLATE BINARY",
    )?;
    let mut rows = statement.query([session_id])?;
    while let Some(row) = rows.next()? {
        for index in 0..3 {
            field(&mut tools, text(row, index)?.as_bytes());
        }
        field(&mut tools, blob(row, 3)?);
        field(&mut tools, text(row, 4)?.as_bytes());
        match row.get_ref(5)? {
            rusqlite::types::ValueRef::Null => field(&mut tools, &[0]),
            _ => {
                field(&mut tools, &[1]);
                field(&mut tools, blob(row, 5)?);
            }
        }
    }
    Ok(CheckpointCut {
        work_revision: u64::try_from(revision).map_err(|_| conflict("invalid work revision"))?,
        event_seq,
        event_digest: events.finalize().into(),
        input_digest: inputs.finalize().into(),
        tool_digest: tools.finalize().into(),
    })
}
pub(crate) fn coverage(
    connection: &Connection,
    session: &str,
    manifest: &CoverageManifest,
) -> Result<()> {
    for (sql, expected) in [
        (
            "SELECT id FROM messages WHERE session_id=?1 AND state='delivered' ORDER BY id COLLATE BINARY",
            &manifest.message_ids,
        ),
        (
            "SELECT call_id FROM tool_calls WHERE session_id=?1 ORDER BY call_id COLLATE BINARY",
            &manifest.tool_call_ids,
        ),
    ] {
        let mut query = connection.prepare(sql)?;
        let mut rows = query.query([session])?;
        for id in expected {
            if rows
                .next()?
                .map(|row| row.get::<_, String>(0))
                .transpose()?
                .as_ref()
                != Some(id)
            {
                return Err(conflict("checkpoint coverage differs from durable work"));
            }
        }
        if rows.next()?.is_some() {
            return Err(conflict("checkpoint coverage omits durable work"));
        }
    }
    Ok(())
}

pub(crate) fn text<'a>(row: &'a rusqlite::Row<'_>, index: usize) -> Result<&'a str> {
    row.get_ref(index)?
        .as_str()
        .map_err(|_| conflict("invalid stored text"))
}
fn blob<'a>(row: &'a rusqlite::Row<'_>, index: usize) -> Result<&'a [u8]> {
    row.get_ref(index)?
        .as_blob()
        .map_err(|_| conflict("invalid stored digest"))
}
/// Borrow only the prompt; unrelated session result/configuration is never copied.
pub(crate) fn with_prompt<T>(
    connection: &Connection,
    session: &str,
    f: impl FnOnce(&str) -> T,
) -> Result<T> {
    let mut query =
        connection.prepare("SELECT json_extract(body,'$.prompt') FROM sessions WHERE id=?1")?;
    let mut rows = query.query([session])?;
    let row = rows
        .next()?
        .ok_or_else(|| foundry_contracts::ContractError::NotFound(session.into()))?;
    Ok(f(text(row, 0)?))
}
