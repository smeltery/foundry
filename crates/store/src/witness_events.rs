use crate::{Result, checkpoint_codec::identifier, conflict};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Sanitized durable event facts; excludes raw event data and private receipts.
pub struct EvidenceEvent {
    pub id: String,
    pub sequence: u64,
    pub revision: u64,
    pub attempt: String,
    pub kind: String,
    pub digest: [u8; 32],
    pub data_bytes: u64,
    pub subject: String,
    pub tool: Option<String>,
    pub encoding: Option<String>,
    pub input_digest: Option<[u8; 32]>,
    pub input_bytes: Option<u64>,
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    let value = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| conflict("unsupported witness event metadata"))?;
    identifier(value)?;
    Ok(value)
}
fn hex(value: &str) -> Result<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(conflict("invalid event input digest"));
    }
    let mut result = [0; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let digit = |b: u8| {
            if b.is_ascii_digit() {
                b - b'0'
            } else {
                b - b'a' + 10
            }
        };
        result[index] = digit(pair[0]) * 16 + digit(pair[1]);
    }
    Ok(result)
}
pub(crate) fn measured_size(connection: &Connection, session: &str) -> Result<usize> {
    let (count,valid,identities):(i64,bool,i64)=connection.query_row("SELECT COUNT(*),COALESCE(MIN(
        typeof(id)='text' AND length(CAST(id AS BLOB)) BETWEEN 1 AND 128
        AND typeof(attempt_id)='text' AND length(CAST(attempt_id AS BLOB))=36
        AND typeof(sequence)='integer' AND sequence>0 AND typeof(revision)='integer' AND revision>=0
        AND typeof(data)='text' AND length(CAST(data AS BLOB)) BETWEEN 1 AND 2097152),1),COALESCE(SUM(length(CAST(id AS BLOB))+length(CAST(attempt_id AS BLOB))+length(CAST(kind AS BLOB))),0)
        FROM events WHERE session_id=?1 AND kind IN ('tool_request','message_accepted','continue_accepted')",[session],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if !valid
        || identities > 262144
        || count > crate::witness::MAX_WITNESS_SELECTED_EVENT_ROWS as i64
    {
        return Err(conflict("witness event evidence exceeds bounds"));
    }
    // Measure every normalized row before allocating the returned evidence vector.
    let mut encoded_size = 2usize;
    visit(
        connection,
        session,
        |event, normalized| {
            let mut counter = EvidenceCounter(0);
            serde_json::to_writer(&mut counter, normalized)
                .map_err(|_| conflict("witness normalized event evidence exceeds byte limit"))?;
            encoded_size = encoded_size
                .checked_add(counter.0 + usize::from(event > 0))
                .filter(|n| *n <= 262144)
                .ok_or_else(|| conflict("witness normalized event evidence exceeds byte limit"))?;
            Ok(())
        },
        None,
    )?;
    Ok(encoded_size)
}

pub(crate) fn selected(connection: &Connection, session: &str) -> Result<Vec<EvidenceEvent>> {
    measured_size(connection, session)?;
    let mut evidence = Vec::new();
    visit(connection, session, |_, _| Ok(()), Some(&mut evidence))?;
    Ok(evidence)
}

fn visit(
    connection: &Connection,
    session: &str,
    mut measure: impl FnMut(usize, &Value) -> Result<()>,
    mut evidence: Option<&mut Vec<EvidenceEvent>>,
) -> Result<()> {
    let mut query=connection.prepare("SELECT id,sequence,revision,attempt_id,kind,data FROM events
        WHERE session_id=?1 AND kind IN ('tool_request','message_accepted','continue_accepted') ORDER BY sequence")?;
    let mut rows = query.query([session])?;
    let mut index = 0;
    while let Some(row) = rows.next()? {
        let id = crate::checkpoint_cut::text(row, 0)?;
        identifier(id)?;
        let attempt = crate::checkpoint_cut::text(row, 3)?;
        identifier(attempt)?;
        if uuid::Uuid::parse_str(attempt).map_or(true, |id| id.to_string() != attempt) {
            return Err(conflict("invalid witness event attempt identity"));
        }
        let kind = crate::checkpoint_cut::text(row, 4)?;
        let data = crate::checkpoint_cut::text(row, 5)?;
        let value: Value = serde_json::from_str(data)?;
        if text(&value, "attempt_id")? != attempt {
            return Err(conflict("event attempt differs from durable owner"));
        }
        let (subject, tool, encoding, input_digest, input_bytes) = match kind {
            "continue_accepted" => {
                let accepted = crate::continue_read::verified(
                    connection,
                    session,
                    text(&value, "restore_id")?,
                )?
                .ok_or_else(|| conflict("Continue event has no acceptance"))?;
                let reference = &accepted.reference;
                let subject = text(&value, "message_id")?;
                if reference.event_id != id
                    || reference.event_sequence != row.get::<_, u64>(1)?
                    || row.get::<_, i64>(2)? != 0
                    || reference.attempt_id != attempt
                    || reference.message_id != subject
                {
                    return Err(conflict("Continue event differs from acceptance"));
                }
                (subject, None, None, None, None)
            }
            "message_accepted" if text(&value, "type")? == "message_accepted" => {
                // Validate the retained event shape without copying its private receipt.
                text(&value, "receipt")?;
                (text(&value, "message_id")?, None, None, None, None)
            }
            "tool_request" if text(&value, "type")? == "tool_call" => {
                if ["input_encoding", "input_bytes", "input_sha256"]
                    .iter()
                    .any(|key| value.get(key).is_none())
                {
                    return Err(foundry_contracts::ContractError::Unsupported(
                        "legacy raw tool event evidence is not available".into(),
                    )
                    .into());
                }
                let encoding = text(&value, "input_encoding")?;
                if encoding != "serde_json.compact.v1" {
                    return Err(conflict("unsupported event input encoding"));
                }
                let bytes = value
                    .get("input_bytes")
                    .and_then(Value::as_u64)
                    .filter(|n| *n > 0 && *n <= 2097152)
                    .ok_or_else(|| conflict("invalid event input length"))?;
                (
                    text(&value, "call_id")?,
                    Some(text(&value, "name")?),
                    Some(encoding),
                    Some(hex(text(&value, "input_sha256")?)?),
                    Some(bytes),
                )
            }
            _ => {
                return Err(foundry_contracts::ContractError::Unsupported(
                    "witness event provenance unavailable".into(),
                )
                .into());
            }
        };
        let sequence = row.get::<_, i64>(1)? as u64;
        let revision = row.get::<_, i64>(2)? as u64;
        let data_digest: [u8; 32] = Sha256::digest(data.as_bytes()).into();
        let normalized = json!({"id":id,"sequence":sequence,"revision":revision,"attempt_id":attempt,"kind":kind,
            "data_digest":data_digest,"data_bytes":data.len(),"subject_id":subject,"tool":tool,
            "input_encoding":encoding,"input_digest":input_digest,"input_bytes":input_bytes});
        measure(index, &normalized)?;
        index += 1;
        if let Some(evidence) = evidence.as_deref_mut() {
            evidence.push(EvidenceEvent {
                id: id.into(),
                sequence,
                revision,
                attempt: attempt.into(),
                kind: kind.into(),
                digest: data_digest,
                data_bytes: data.len() as u64,
                subject: subject.into(),
                tool: tool.map(str::to_owned),
                encoding: encoding.map(str::to_owned),
                input_digest,
                input_bytes,
            });
        }
    }
    Ok(())
}

struct EvidenceCounter(usize);
impl std::io::Write for EvidenceCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|n| *n <= 262144)
            .ok_or_else(|| std::io::Error::other("event evidence byte limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
