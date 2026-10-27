use crate::{
    CheckpointRef, ContextCheckpointId, MAX_CHECKPOINT_BYTES, MAX_CHECKPOINT_COVERAGE_BYTES,
    MAX_CHECKPOINT_METADATA_BYTES, PrivateCheckpoint, Result, checkpoint_codec,
    checkpoint_cut::digest, conflict,
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

pub(crate) struct Stored {
    pub id: String,
    pub session: String,
    pub attempt: String,
    pub generation: i64,
    pub parent: Option<String>,
    pub request: String,
    pub metadata: String,
    pub coverage: String,
    pub configuration_digest: [u8; 32],
    pub payload: Vec<u8>,
    pub payload_digest: [u8; 32],
    pub request_digest: [u8; 32],
    pub record_digest: [u8; 32],
    pub committed: i64,
    pub request_format: i64,
    pub witness: Option<crate::witness_storage::StoredWitness>,
}
impl Stored {
    pub fn request_hash(&self) -> [u8; 32] {
        let payload_length = (self.payload.len() as u64).to_be_bytes();
        checkpoint_codec::request_hash(
            [
                self.session.as_bytes(),
                self.attempt.as_bytes(),
                self.request.as_bytes(),
                self.parent.as_deref().unwrap_or("").as_bytes(),
                self.metadata.as_bytes(),
                self.coverage.as_bytes(),
                &self.configuration_digest,
                &self.payload_digest,
                &payload_length,
            ],
            self.witness
                .as_ref()
                .map(|w| (w.encoded.digest(), w.encoded.bytes().len())),
        )
    }
    pub fn record_hash(&self) -> [u8; 32] {
        digest(
            b"foundry.checkpoint.record.v1",
            &[
                &self.request_digest,
                self.id.as_bytes(),
                &self.generation.to_be_bytes(),
                &self.committed.to_be_bytes(),
            ],
        )
    }
    fn verify(&self) -> Result<()> {
        crate::witness_storage::check_row(self)?;
        if <[u8; 32]>::from(Sha256::digest(&self.payload)) != self.payload_digest
            || self.request_hash() != self.request_digest
            || self.record_hash() != self.record_digest
        {
            return Err(conflict("checkpoint integrity mismatch"));
        }
        Ok(())
    }
    fn reference(&self) -> Result<CheckpointRef> {
        let (format, cut, _, _) = checkpoint_codec::decode(&self.metadata, &self.coverage)?;
        Ok(CheckpointRef {
            id: ContextCheckpointId::parse(&self.id)?,
            session_id: self.session.clone(),
            source_attempt_id: self.attempt.clone(),
            parent_id: self
                .parent
                .as_deref()
                .map(ContextCheckpointId::parse)
                .transpose()?,
            generation: u64::try_from(self.generation)
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| conflict("invalid checkpoint generation"))?,
            format,
            cut,
            configuration_digest: self.configuration_digest,
            payload_digest: self.payload_digest,
            request_digest: self.request_digest,
            record_digest: self.record_digest,
            payload_length: self.payload.len() as u64,
            committed_ms: self.committed,
        })
    }
    pub fn private(self) -> Result<PrivateCheckpoint> {
        self.private_and_witness().map(|(private, _)| private)
    }
    pub fn private_and_witness(
        self,
    ) -> Result<(
        PrivateCheckpoint,
        Option<crate::witness_storage::StoredWitness>,
    )> {
        self.verify()?;
        let reference = self.reference()?;
        let (_, _, coverage, producer_validation) =
            checkpoint_codec::decode(&self.metadata, &self.coverage)?;
        Ok((
            PrivateCheckpoint {
                reference,
                payload: self.payload,
                coverage,
                producer_validation,
            },
            self.witness,
        ))
    }
}
pub(crate) fn load(connection: &Connection, session: &str, id: &str) -> Result<Option<Stored>> {
    let sizes: Option<(i64, i64, i64, bool)> = connection.query_row(
        "SELECT length(payload),length(CAST(metadata AS BLOB)),length(CAST(coverage AS BLOB)),
            typeof(id)='text' AND length(CAST(id AS BLOB))=36
            AND typeof(session_id)='text' AND length(CAST(session_id AS BLOB))=36
            AND typeof(attempt_id)='text' AND length(CAST(attempt_id AS BLOB))=36
            AND typeof(request_id)='text' AND length(CAST(request_id AS BLOB)) BETWEEN 1 AND 128
            AND (parent_id IS NULL OR (typeof(parent_id)='text' AND length(CAST(parent_id AS BLOB))=36))
         FROM context_checkpoints WHERE session_id=?1 AND id=?2",
        params![session, id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).optional()?;
    let Some((payload, metadata, coverage, identities_valid)) = sizes else {
        return Ok(None);
    };
    if !identities_valid
        || !(1..=MAX_CHECKPOINT_BYTES as i64).contains(&payload)
        || !(1..=MAX_CHECKPOINT_METADATA_BYTES as i64).contains(&metadata)
        || !(1..=MAX_CHECKPOINT_COVERAGE_BYTES as i64).contains(&coverage)
    {
        return Err(conflict("checkpoint stored size limit"));
    }
    let mut stored = connection.query_row(
        "SELECT id,session_id,attempt_id,generation,parent_id,request_id,metadata,coverage,
                payload,payload_digest,request_digest,record_digest,committed_ms,configuration_digest,request_format
         FROM context_checkpoints WHERE session_id=?1 AND id=?2",
        params![session, id],
        |row| Ok(Stored {
            id: row.get(0)?, session: row.get(1)?, attempt: row.get(2)?,
            generation: row.get(3)?, parent: row.get(4)?, request: row.get(5)?,
            metadata: row.get(6)?, coverage: row.get(7)?, payload: row.get(8)?,
            payload_digest: row.get(9)?, request_digest: row.get(10)?,
            record_digest: row.get(11)?, committed: row.get(12)?,
            configuration_digest: row.get(13)?, request_format: row.get(14)?, witness: None,
        }),
    )?;
    stored.witness = crate::witness_storage::load(connection, &stored)?;
    Ok(Some(stored))
}

pub(crate) fn head(connection: &Connection, session: &str) -> Result<Option<Stored>> {
    let generation: Option<i64> = connection.query_row(
        "SELECT MAX(generation) FROM context_checkpoints WHERE session_id=?1",
        [session],
        |row| row.get(0),
    )?;
    let id: Option<Option<String>> = connection.query_row(
        "SELECT CASE WHEN typeof(checkpoint_id)='text' AND length(CAST(checkpoint_id AS BLOB))=36
         THEN checkpoint_id ELSE NULL END FROM context_checkpoint_heads WHERE session_id=?1",
        [session], |row| row.get(0),
    ).optional()?;
    match (id, generation) {
        (None, None) => Ok(None),
        (Some(Some(id)), Some(generation)) => {
            let row = load(connection, session, &id)?
                .ok_or_else(|| conflict("checkpoint head is missing"))?;
            if row.generation != generation {
                return Err(conflict("checkpoint head is not the latest generation"));
            }
            Ok(Some(row))
        }
        _ => Err(conflict("checkpoint head is missing or invalid")),
    }
}
pub(crate) fn by_request(
    connection: &Connection,
    session: &str,
    request: &str,
) -> Result<Option<Stored>> {
    let id: Option<Option<String>> = connection
        .query_row(
            "SELECT CASE WHEN typeof(id)='text' AND length(CAST(id AS BLOB))=36
             THEN id ELSE NULL END FROM context_checkpoints WHERE session_id=?1 AND request_id=?2",
            params![session, request],
            |r| r.get(0),
        )
        .optional()?;
    match id {
        Some(Some(id)) => load(connection, session, &id),
        Some(None) => Err(conflict("invalid checkpoint identity")),
        None => Ok(None),
    }
}
