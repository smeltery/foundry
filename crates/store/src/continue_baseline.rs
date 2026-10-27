//! Bounded baseline integrity without witness-fact traversal or ancestor recursion.
use crate::{
    ContinueAcceptanceInput, Result, checkpoint_codec, checkpoint_cut::digest, conflict,
    restore_read,
};
use rusqlite::{Connection, params};

pub(crate) fn verify(
    db: &Connection,
    input: &ContinueAcceptanceInput,
    restore: &restore_read::Row,
) -> Result<()> {
    let i = &input.intent;
    if restore.session != i.session_id
        || restore.configuration != i.configuration_digest
        || restore.checkpoint != i.baseline_checkpoint_id.as_str()
        || restore.receipt()?.as_ref() != Some(&input.authorization)
    {
        return Err(conflict("acceptance differs from immutable authorization"));
    }
    let valid: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM context_checkpoints c JOIN context_witnesses w ON w.checkpoint_id=c.id
        WHERE c.id=?1 AND c.session_id=?2 AND w.session_id=c.session_id
        AND typeof(c.id)='text' AND length(CAST(c.id AS BLOB))=36
        AND typeof(c.session_id)='text' AND length(CAST(c.session_id AS BLOB))=36
        AND typeof(w.checkpoint_id)='text' AND length(CAST(w.checkpoint_id AS BLOB))=36
        AND typeof(w.session_id)='text' AND length(CAST(w.session_id AS BLOB))=36
        AND typeof(c.request_format)='integer' AND c.request_format=2
        AND typeof(c.generation)='integer' AND c.generation>0 AND typeof(w.checkpoint_generation)='integer' AND w.checkpoint_generation=c.generation
        AND typeof(c.committed_ms)='integer' AND c.committed_ms>=0
        AND typeof(c.attempt_id)='text' AND length(CAST(c.attempt_id AS BLOB))=36
        AND typeof(c.request_id)='text' AND length(CAST(c.request_id AS BLOB)) BETWEEN 1 AND 128
        AND (c.parent_id IS NULL OR (typeof(c.parent_id)='text' AND length(CAST(c.parent_id AS BLOB))=36))
        AND typeof(c.metadata)='text' AND length(CAST(c.metadata AS BLOB)) BETWEEN 1 AND 16384
        AND typeof(c.coverage)='text' AND length(CAST(c.coverage AS BLOB)) BETWEEN 1 AND 65536
        AND typeof(c.payload)='blob' AND length(c.payload) BETWEEN 1 AND 1048576
        AND typeof(c.payload_digest)='blob' AND length(c.payload_digest)=32
        AND typeof(c.configuration_digest)='blob' AND length(c.configuration_digest)=32
        AND typeof(c.request_digest)='blob' AND length(c.request_digest)=32
        AND typeof(c.record_digest)='blob' AND length(c.record_digest)=32
        AND typeof(w.checkpoint_record_digest)='blob' AND w.checkpoint_record_digest=c.record_digest AND typeof(w.format)='integer' AND w.format=1
        AND typeof(w.body)='blob' AND length(w.body) BETWEEN 1 AND 262144
        AND typeof(w.body_digest)='blob' AND length(w.body_digest)=32
        AND typeof(w.association_digest)='blob' AND length(w.association_digest)=32)",
        params![i.baseline_checkpoint_id.as_str(),i.session_id],|r|r.get(0))?;
    if !valid {
        return Err(conflict("acceptance baseline witness metadata invalid"));
    }
    struct Baseline {
        attempt: String,
        request: String,
        parent: Option<String>,
        metadata: String,
        coverage: String,
        payload_digest: [u8; 32],
        payload_length: u64,
        configuration: [u8; 32],
        request_digest: [u8; 32],
        record: [u8; 32],
        generation: u64,
        committed: i64,
        body_length: u64,
        witness: [u8; 32],
        association: [u8; 32],
    }
    let b = db.query_row("SELECT c.attempt_id,c.request_id,c.parent_id,c.metadata,c.coverage,c.payload_digest,length(c.payload),
        c.configuration_digest,c.request_digest,c.record_digest,c.generation,c.committed_ms,length(w.body),w.body_digest,w.association_digest
        FROM context_checkpoints c JOIN context_witnesses w ON w.checkpoint_id=c.id WHERE c.id=?1 AND c.session_id=?2",
        params![i.baseline_checkpoint_id.as_str(),i.session_id],|r|Ok(Baseline {
            attempt:r.get(0)?,request:r.get(1)?,parent:r.get(2)?,metadata:r.get(3)?,coverage:r.get(4)?,
            payload_digest:r.get(5)?,payload_length:r.get(6)?,configuration:r.get(7)?,request_digest:r.get(8)?,
            record:r.get(9)?,generation:r.get(10)?,committed:r.get(11)?,body_length:r.get(12)?,witness:r.get(13)?,association:r.get(14)?,
        }))?;
    crate::continue_intent::uuid(&b.attempt)?;
    checkpoint_codec::identifier(&b.request)?;
    if let Some(parent) = &b.parent {
        crate::continue_intent::uuid(parent)?;
    }
    checkpoint_codec::decode(&b.metadata, &b.coverage)?;
    let terminal = checkpoint_codec::decode_terminal(&b.metadata)?;
    crate::witness_binding::terminal_digest(&terminal)?;
    // Historical references authenticate the stored envelope, not payload/native semantics.
    // Never deserialize this baseline's witness or traverse its own retained instructions.
    let request = checkpoint_codec::request_hash(
        [
            i.session_id.as_bytes(),
            b.attempt.as_bytes(),
            b.request.as_bytes(),
            b.parent.as_deref().unwrap_or("").as_bytes(),
            b.metadata.as_bytes(),
            b.coverage.as_bytes(),
            &b.configuration,
            &b.payload_digest,
            &b.payload_length.to_be_bytes(),
        ],
        Some((b.witness, b.body_length as usize)),
    );
    let record = digest(
        b"foundry.checkpoint.record.v1",
        &[
            &request,
            i.baseline_checkpoint_id.as_str().as_bytes(),
            &b.generation.to_be_bytes(),
            &b.committed.to_be_bytes(),
        ],
    );
    let association = digest(
        b"foundry.checkpoint.witness-association.v1",
        &[
            i.session_id.as_bytes(),
            i.baseline_checkpoint_id.as_str().as_bytes(),
            &b.generation.to_be_bytes(),
            &record,
            &1_u64.to_be_bytes(),
            &b.witness,
            &b.body_length.to_be_bytes(),
        ],
    );
    if request != b.request_digest
        || request != restore.checkpoint_digest
        || record != b.record
        || record != i.baseline_record_digest
        || b.witness != i.baseline_witness_digest
        || association != b.association
        || b.configuration != i.configuration_digest
    {
        return Err(conflict("acceptance baseline integrity mismatch"));
    }
    Ok(())
}
