use crate::{
    Result,
    checkpoint_cut::digest,
    checkpoint_read::Stored,
    conflict,
    witness::{EncodedWitness, WitnessBodyV1},
};
use rusqlite::{Connection, OptionalExtension, params};
pub(crate) struct StoredWitness {
    pub body: WitnessBodyV1,
    pub encoded: EncodedWitness,
}
pub(crate) use crate::witness_binding::{check_input, check_row};

fn association(row: &Stored, witness: &StoredWitness) -> [u8; 32] {
    association_fields(
        &row.session,
        &row.id,
        row.generation,
        row.record_digest,
        witness.encoded.digest(),
        witness.encoded.bytes().len() as i64,
    )
}
fn association_fields(
    session: &str,
    id: &str,
    generation: i64,
    record: [u8; 32],
    body: [u8; 32],
    length: i64,
) -> [u8; 32] {
    digest(
        b"foundry.checkpoint.witness-association.v1",
        &[
            session.as_bytes(),
            id.as_bytes(),
            &(generation as u64).to_be_bytes(),
            &record,
            &1_u64.to_be_bytes(),
            &body,
            &(length as u64).to_be_bytes(),
        ],
    )
}
pub(crate) fn insert(connection: &Connection, row: &Stored) -> Result<()> {
    if let Some(w) = &row.witness {
        let changed=connection.execute("INSERT INTO context_witnesses(checkpoint_id,session_id,format,body,body_digest,checkpoint_generation,checkpoint_record_digest,association_digest)
            VALUES(?1,?2,1,?3,?4,?5,?6,?7)",params![row.id,row.session,w.encoded.bytes(),w.encoded.digest().as_slice(),row.generation,row.record_digest.as_slice(),association(row,w).as_slice()])?;
        if changed != 1 {
            return Err(conflict("witness insertion failed"));
        }
    }
    Ok(())
}
pub(crate) fn load(connection: &Connection, row: &Stored) -> Result<Option<StoredWitness>> {
    let valid:Option<bool>=connection.query_row("SELECT typeof(session_id)='text' AND session_id=?2 AND length(CAST(checkpoint_id AS BLOB))=36
        AND typeof(format)='integer' AND format=1 AND typeof(body)='blob' AND length(body) BETWEEN 1 AND 262144
        AND typeof(body_digest)='blob' AND length(body_digest)=32
        AND typeof(checkpoint_generation)='integer' AND checkpoint_generation=?3
        AND typeof(checkpoint_record_digest)='blob' AND checkpoint_record_digest=?4
        AND typeof(association_digest)='blob' AND length(association_digest)=32
        FROM context_witnesses WHERE checkpoint_id=?1",
        params![row.id,row.session,row.generation,row.record_digest.as_slice()],|r|r.get(0)).optional()?;
    match (row.request_format, valid) {
        (1, None) => return Ok(None),
        (2, Some(true)) => {}
        _ => {
            return Err(conflict(
                "checkpoint witness format or association mismatch",
            ));
        }
    }
    let (bytes, expected, link): (Vec<u8>, [u8; 32], [u8; 32]) = connection.query_row(
        "SELECT body,body_digest,association_digest FROM context_witnesses WHERE checkpoint_id=?1",
        [&row.id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let body = WitnessBodyV1::decode(&bytes)?;
    let witness = StoredWitness {
        encoded: body.encode()?,
        body,
    };
    if witness.encoded.digest() != expected || association(row, &witness) != link {
        return Err(conflict("witness integrity mismatch"));
    }
    check_parent(connection, row, &witness.body)?;
    Ok(Some(witness))
}

/// Reads only one parent association/header; never parent payload/body or ancestry.
pub(crate) fn check_parent(
    connection: &Connection,
    row: &Stored,
    body: &WitnessBodyV1,
) -> Result<()> {
    let Some(parent) = &body.header.parent else {
        if row.parent.is_some() || row.generation != 1 {
            return Err(conflict("witness root has a parent or nonroot generation"));
        }
        return Ok(());
    };
    if row.parent.as_deref() != Some(parent.checkpoint_id.as_str())
        || parent.generation.checked_add(1) != Some(row.generation as u64)
    {
        return Err(conflict("witness parent identity mismatch"));
    }
    let valid:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM context_checkpoints c JOIN context_witnesses w ON w.checkpoint_id=c.id
        WHERE c.id=?1 AND c.session_id=?2 AND w.session_id=c.session_id AND c.request_format=2
        AND typeof(c.id)='text' AND length(CAST(c.id AS BLOB))=36
        AND typeof(c.generation)='integer' AND c.generation>0 AND w.checkpoint_generation=c.generation
        AND typeof(c.committed_ms)='integer' AND typeof(c.request_digest)='blob' AND length(c.request_digest)=32
        AND typeof(c.record_digest)='blob' AND length(c.record_digest)=32 AND w.checkpoint_record_digest=c.record_digest
        AND typeof(w.format)='integer' AND w.format=1 AND typeof(w.body)='blob' AND length(w.body) BETWEEN 1 AND 262144
        AND typeof(w.body_digest)='blob' AND length(w.body_digest)=32 AND typeof(w.association_digest)='blob' AND length(w.association_digest)=32)",
        params![parent.checkpoint_id,row.session],|r|r.get(0))?;
    if !valid {
        return Err(conflict(
            "required parent witness association missing or invalid",
        ));
    }
    type ParentEnvelope = (i64, [u8; 32], [u8; 32], i64, [u8; 32], i64, [u8; 32]);
    let (generation,record,request,committed,body_digest,length,link):ParentEnvelope=connection.query_row(
        "SELECT c.generation,c.record_digest,c.request_digest,c.committed_ms,w.body_digest,length(w.body),w.association_digest
         FROM context_checkpoints c JOIN context_witnesses w ON w.checkpoint_id=c.id WHERE c.id=?1 AND c.session_id=?2",
        params![parent.checkpoint_id,row.session],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
    if parent.generation != generation as u64
        || parent.record_digest != record
        || parent.witness_digest != body_digest
        || digest(
            b"foundry.checkpoint.record.v1",
            &[
                &request,
                parent.checkpoint_id.as_bytes(),
                &generation.to_be_bytes(),
                &committed.to_be_bytes(),
            ],
        ) != record
        || association_fields(
            &row.session,
            &parent.checkpoint_id,
            generation,
            record,
            body_digest,
            length,
        ) != link
    {
        return Err(conflict("parent witness association integrity mismatch"));
    }
    Ok(())
}

pub(crate) fn check_quota_associations(connection: &Connection) -> Result<()> {
    let invalid:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM context_checkpoints c LEFT JOIN context_witnesses w ON w.checkpoint_id=c.id
        WHERE typeof(c.request_format)!='integer' OR c.request_format NOT IN (1,2)
         OR (c.request_format=1 AND w.checkpoint_id IS NOT NULL) OR (c.request_format=2 AND w.checkpoint_id IS NULL))
        OR EXISTS(SELECT 1 FROM context_witnesses w LEFT JOIN context_checkpoints c ON c.id=w.checkpoint_id
        WHERE c.id IS NULL OR w.session_id!=c.session_id OR typeof(w.body)!='blob' OR length(w.body) NOT BETWEEN 1 AND 262144
         OR typeof(w.checkpoint_id)!='text' OR length(CAST(w.checkpoint_id AS BLOB))!=36
         OR typeof(w.session_id)!='text' OR length(CAST(w.session_id AS BLOB))!=36
         OR typeof(w.format)!='integer' OR w.format!=1 OR typeof(w.body_digest)!='blob' OR length(w.body_digest)!=32
         OR typeof(w.checkpoint_generation)!='integer' OR w.checkpoint_generation!=c.generation
         OR typeof(w.checkpoint_record_digest)!='blob' OR length(w.checkpoint_record_digest)!=32
         OR typeof(w.association_digest)!='blob' OR length(w.association_digest)!=32)",[],|r|r.get(0))?;
    if invalid {
        return Err(conflict("invalid witness quota association"));
    }
    Ok(())
}
