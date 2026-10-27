use crate::{CheckpointRef, Result, checkpoint_cut, conflict, witness_events};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Write;

pub(super) const MAX_EVIDENCE_BYTES: usize = 262144;

/// Measure the complete sanitized evidence projection before copying returned facts.
/// Payload and native witness allocations have independent existing bounds.
pub(super) fn preflight(db: &Connection, session: &str, head: &CheckpointRef) -> Result<()> {
    let mut count = Counter(0);
    count.raw(br#"{"facts":{"head":"#)?;
    count.value(&json!({
        "id":head.id.as_str(),"session_id":head.session_id,
        "source_attempt_id":head.source_attempt_id,
        "parent_id":head.parent_id.as_ref().map(|id| id.as_str()),
        "generation":head.generation,"format":{
            "format_id":head.format.format_id,"format_version":head.format.format_version,
            "profile_id":head.format.profile_id,"binding_id":head.format.binding_id,
            "binding_version":head.format.binding_version},
        "cut":cut(head),"configuration_digest":head.configuration_digest,
        "payload_digest":head.payload_digest,"request_digest":head.request_digest,
        "record_digest":head.record_digest,"payload_length":head.payload_length,
        "committed_ms":head.committed_ms
    }))?;
    count.raw(br#","cut":"#)?;
    count.value(&cut(head))?;
    count.raw(br#","coverage":{"message_ids":["#)?;
    ids(
        db,
        session,
        "SELECT id FROM messages WHERE session_id=?1 AND state='delivered' ORDER BY id COLLATE BINARY",
        &mut count,
    )?;
    count.raw(br#"],"tool_call_ids":["#)?;
    ids(
        db,
        session,
        "SELECT call_id FROM tool_calls WHERE session_id=?1 ORDER BY call_id COLLATE BINARY",
        &mut count,
    )?;
    count.raw(b"]}")?;
    let (digest, bytes) = checkpoint_cut::with_prompt(db, session, |text| {
        (
            <[u8; 32]>::from(Sha256::digest(text.as_bytes())),
            text.len(),
        )
    })?;
    count.raw(br#","prompt_digest":"#)?;
    count.value(&json!(digest))?;
    count.raw(br#","prompt_bytes":"#)?;
    count.value(&json!(bytes))?;
    count.raw(br#","messages":["#)?;
    rows(
        db,
        session,
        "SELECT id,sequence,attempt_id,text FROM messages WHERE session_id=?1 AND state='delivered' ORDER BY id COLLATE BINARY",
        &mut count,
        |row| {
            let text = checkpoint_cut::text(row, 3)?;
            Ok(
                json!({"id":checkpoint_cut::text(row,0)?,"sequence":row.get::<_,i64>(1)?,
            "attempt_id":checkpoint_cut::text(row,2)?,
            "text_digest":<[u8;32]>::from(Sha256::digest(text.as_bytes())),"text_bytes":text.len()}),
            )
        },
    )?;
    count.raw(br#"],"tools":["#)?;
    rows(
        db,
        session,
        "SELECT call_id,attempt_id,tool,input_digest,outcome,output_digest FROM tool_calls WHERE session_id=?1 ORDER BY call_id COLLATE BINARY",
        &mut count,
        |row| {
            Ok(
                json!({"call_id":checkpoint_cut::text(row,0)?,"attempt_id":checkpoint_cut::text(row,1)?,
            "tool":checkpoint_cut::text(row,2)?,"input_digest":row.get::<_,[u8;32]>(3)?,
            "outcome":checkpoint_cut::text(row,4)?,"output_digest":row.get::<_,Option<[u8;32]>>(5)?}),
            )
        },
    )?;
    count.raw(br#"]},"events":"#)?;
    count.add(witness_events::measured_size(db, session)?)?;
    count.raw(b"}")?;
    Ok(())
}

fn cut(head: &CheckpointRef) -> Value {
    json!({"work_revision":head.cut.work_revision,"event_seq":head.cut.event_seq,
        "event_digest":head.cut.event_digest,"input_digest":head.cut.input_digest,
        "tool_digest":head.cut.tool_digest})
}

fn rows(
    db: &Connection,
    session: &str,
    sql: &str,
    count: &mut Counter,
    normalize: impl Fn(&rusqlite::Row<'_>) -> Result<Value>,
) -> Result<()> {
    let mut query = db.prepare(sql)?;
    let mut rows = query.query([session])?;
    let mut first = true;
    while let Some(row) = rows.next()? {
        if !first {
            count.raw(b",")?;
        }
        first = false;
        count.value(&normalize(row)?)?;
    }
    Ok(())
}

fn ids(db: &Connection, session: &str, sql: &str, count: &mut Counter) -> Result<()> {
    rows(db, session, sql, count, |row| {
        Ok(json!(checkpoint_cut::text(row, 0)?))
    })
}

struct Counter(usize);
impl Counter {
    fn add(&mut self, bytes: usize) -> Result<()> {
        self.0 = self
            .0
            .checked_add(bytes)
            .filter(|n| *n <= MAX_EVIDENCE_BYTES)
            .ok_or_else(|| conflict("restore combined evidence byte limit"))?;
        Ok(())
    }
    fn raw(&mut self, bytes: &[u8]) -> Result<()> {
        self.add(bytes.len())
    }
    fn value(&mut self, value: &Value) -> Result<()> {
        serde_json::to_writer(self, value)
            .map_err(|_| conflict("restore combined evidence byte limit"))
    }
}
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.add(bytes.len())
            .map_err(|_| std::io::Error::other("restore evidence limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_counter_is_inclusive_and_counts_json_escaping() {
        let value = json!("quote\" newline\n é 😀");
        let encoded = serde_json::to_vec(&value).unwrap();
        let mut count = Counter(MAX_EVIDENCE_BYTES - encoded.len());
        count.value(&value).unwrap();
        assert_eq!(count.0, MAX_EVIDENCE_BYTES);
        assert!(count.raw(b"x").is_err());
        assert!(Counter(usize::MAX).add(1).is_err());
    }
}
