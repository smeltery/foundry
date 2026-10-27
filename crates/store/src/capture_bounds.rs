use crate::{
    MAX_CHECKPOINT_COVERAGE_BYTES, Result, capture_snapshot::MAX_CAPTURE_EVIDENCE_ENTRIES,
    checkpoint_cut::text, conflict,
};
use foundry_contracts::{MAX_FRAME_BYTES, MAX_PROMPT_BYTES};
use rusqlite::{Connection, params};
use std::io::Write;

pub(crate) fn preflight(connection: &Connection, session: &str) -> Result<(usize, usize)> {
    let (messages, message_bytes): (i64, i64) = connection.query_row(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(id AS BLOB))+length(CAST(attempt_id AS BLOB))),0)
         FROM messages WHERE session_id=?1 AND state='delivered'",
        [session],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (tools,tool_bytes):(i64,i64) = connection.query_row(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(call_id AS BLOB))+length(CAST(attempt_id AS BLOB))+length(CAST(tool AS BLOB))),0)
         FROM tool_calls WHERE session_id=?1",[session],|r|Ok((r.get(0)?,r.get(1)?)),
    )?;
    if messages
        .checked_add(tools)
        .and_then(|n| n.checked_add(1))
        .is_none_or(|n| n > MAX_CAPTURE_EVIDENCE_ENTRIES as i64)
        || message_bytes
            .checked_add(tool_bytes)
            .is_none_or(|n| n > MAX_CHECKPOINT_COVERAGE_BYTES as i64)
    {
        return Err(conflict("capture evidence count or identity byte limit"));
    }
    let invalid:bool = connection.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM sessions WHERE id=?1
             AND json_type(body,'$.prompt')='text'
             AND length(CAST(json_extract(body,'$.prompt') AS BLOB)) BETWEEN 1 AND ?2)
         OR EXISTS(SELECT 1 FROM messages WHERE session_id=?1 AND state='delivered' AND NOT (
             typeof(id)='text' AND length(CAST(id AS BLOB)) BETWEEN 1 AND 128
             AND typeof(attempt_id)='text' AND length(CAST(attempt_id AS BLOB))=36
             AND typeof(sequence)='integer' AND sequence>0
             AND typeof(text)='text' AND length(CAST(text AS BLOB)) BETWEEN 1 AND ?2))
         OR EXISTS(SELECT 1 FROM tool_calls WHERE session_id=?1 AND NOT (
             typeof(call_id)='text' AND length(CAST(call_id AS BLOB)) BETWEEN 1 AND 128
             AND typeof(attempt_id)='text' AND length(CAST(attempt_id AS BLOB))=36
             AND typeof(tool)='text' AND length(CAST(tool AS BLOB)) BETWEEN 1 AND 64
             AND typeof(input_digest)='blob' AND length(input_digest)=32
             AND (outcome IS NULL OR (typeof(outcome)='text' AND length(CAST(outcome AS BLOB))<=16))
             AND (output_digest IS NULL OR (typeof(output_digest)='blob' AND length(output_digest)=32))))
         OR EXISTS(SELECT 1 FROM events WHERE session_id=?1 AND NOT (
             typeof(id)='text' AND length(CAST(id AS BLOB)) BETWEEN 1 AND 128
             AND typeof(attempt_id)='text' AND length(CAST(attempt_id AS BLOB))=36
             AND typeof(kind)='text' AND length(CAST(kind AS BLOB)) BETWEEN 1 AND 128
             AND typeof(sequence)='integer' AND sequence>0
             AND typeof(revision)='integer' AND revision>=0
             AND typeof(data)='text' AND length(CAST(data AS BLOB))<=?3))",
        params![session, MAX_PROMPT_BYTES as i64, MAX_FRAME_BYTES as i64],|r|r.get(0),
    )?;
    if invalid {
        return Err(conflict("capture stored field type or byte limit"));
    }
    // No owned identity strings are allocated during validation or JSON-size measurement.
    for sql in [
        "SELECT id,attempt_id FROM messages WHERE session_id=?1 AND state='delivered'",
        "SELECT call_id,attempt_id,tool FROM tool_calls WHERE session_id=?1",
        "SELECT id,attempt_id,kind FROM events WHERE session_id=?1",
    ] {
        let mut query = connection.prepare(sql)?;
        let mut rows = query.query([session])?;
        while let Some(row) = rows.next()? {
            for index in 0..row.as_ref().column_count() {
                let value = text(row, index)?;
                if value.trim().is_empty() || value.chars().any(char::is_control) {
                    return Err(conflict("invalid capture identity"));
                }
            }
        }
    }
    let mut counter = CoverageCounter(0);
    counter.append(br#"{"message_ids":["#)?;
    count_ids(
        connection,
        session,
        "SELECT id FROM messages WHERE session_id=?1 AND state='delivered'",
        &mut counter,
    )?;
    counter.append(br#"],"tool_call_ids":["#)?;
    count_ids(
        connection,
        session,
        "SELECT call_id FROM tool_calls WHERE session_id=?1",
        &mut counter,
    )?;
    counter.append(b"]}")?;
    Ok((
        usize::try_from(messages).map_err(|_| conflict("invalid capture count"))?,
        usize::try_from(tools).map_err(|_| conflict("invalid capture count"))?,
    ))
}
fn count_ids(
    connection: &Connection,
    session: &str,
    sql: &str,
    counter: &mut CoverageCounter,
) -> Result<()> {
    let mut query = connection.prepare(sql)?;
    let mut rows = query.query([session])?;
    let mut first = true;
    while let Some(row) = rows.next()? {
        if !first {
            counter.append(b",")?;
        }
        first = false;
        serde_json::to_writer(&mut *counter, text(row, 0)?)
            .map_err(|_| conflict("capture encoded coverage limit"))?;
    }
    Ok(())
}
struct CoverageCounter(usize);
impl CoverageCounter {
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        self.write_all(bytes)
            .map_err(|_| conflict("capture encoded coverage limit"))
    }
}
impl Write for CoverageCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_CHECKPOINT_COVERAGE_BYTES)
            .ok_or_else(|| std::io::Error::other("coverage limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
