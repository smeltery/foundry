use super::*;
use iroh::{EndpointId, Signature};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
const APP: i64 = 0x4635544d;
const TABLE: &str = "CREATE TABLE checkpoint (id INTEGER PRIMARY KEY CHECK(id=1),payload BLOB NOT NULL,signature BLOB NOT NULL CHECK(length(signature)=64))";
const DOMAIN: &[u8] = b"f5y/local-team-checkpoint/v2\0";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Checkpoint {
    version: u32,
    pub incarnation: [u8; 32],
    pub(super) identity: EndpointId,
    policy: [u8; 32],
    pub revision: u64,
    pub parent: [u8; 32],
    #[serde(rename = "record_validation_horizon_ms")]
    pub accepted_ms: i64,
    pub state: StoredState,
    pub operation_ms: i64,
    intents: Vec<()>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum StoredState {
    Empty,
    Active { document: Box<TeamDocument> },
    Removed { team: String, founder: EndpointId },
}
impl Checkpoint {
    pub fn document(&self) -> Option<&TeamDocument> {
        match &self.state {
            StoredState::Active { document } => Some(document),
            _ => None,
        }
    }

    pub fn empty(incarnation: [u8; 32], identity: EndpointId, policy: [u8; 32]) -> Self {
        Self {
            version: 2,
            incarnation,
            identity,
            policy,
            revision: 0,
            parent: [0; 32],
            accepted_ms: 0,
            state: StoredState::Empty,
            operation_ms: 0,
            intents: Vec::new(),
        }
    }
}
pub(super) fn digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}
pub(super) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > MAX_CHECKPOINT_BYTES - self.0.len() {
                return Err(io::Error::other("checkpoint limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded(Vec::new());
    serde_json::to_writer(&mut output, value).map_err(|_| StoreError::TooLarge)?;
    #[cfg(test)]
    FORBIDDEN.with(|values| {
        for value in values.borrow().iter() {
            assert!(
                !output.0.windows(value.len()).any(|w| w == value),
                "secret reached checkpoint encoding"
            );
        }
    });
    Ok(output.0)
}
pub(super) fn preimage(bytes: &[u8], version: i64) -> Vec<u8> {
    let mut output = if version == 1 {
        b"f5y/local-team-checkpoint/v1\0".to_vec()
    } else {
        DOMAIN.to_vec()
    };
    output.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    output.extend_from_slice(bytes);
    output
}
fn schema(connection: &Connection, expected_version: i64) -> Result<()> {
    let app: i64 = connection.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let count: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    if app != APP || version != expected_version || count != 1 {
        return Err(StoreError::Schema);
    }
    let size: i64 = connection
        .query_row(
            "SELECT length(sql) FROM sqlite_schema WHERE type='table' AND name='checkpoint'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Schema)?;
    if size != TABLE.len() as i64 {
        return Err(StoreError::Schema);
    }
    let statement: String = connection.query_row(
        "SELECT sql FROM sqlite_schema WHERE type='table' AND name='checkpoint'",
        [],
        |r| r.get(0),
    )?;
    if statement != TABLE {
        return Err(StoreError::Schema);
    }
    Ok(())
}
pub(super) fn create(
    connection: &mut Connection,
    checkpoint: &Checkpoint,
    identity: &LocalIdentity,
) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let count: i64 = tx.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))?;
    if count != 0 {
        return Err(StoreError::Schema);
    }
    tx.execute_batch(TABLE)?;
    tx.pragma_update(None, "application_id", APP)?;
    tx.pragma_update(None, "user_version", 2)?;
    write(&tx, checkpoint, identity, true)?;
    tx.commit()?;
    Ok(())
}
pub(super) fn write(
    connection: &Connection,
    checkpoint: &Checkpoint,
    identity: &LocalIdentity,
    insert: bool,
) -> Result<[u8; 32]> {
    let bytes = encode(checkpoint)?;
    let signature = identity.secret_key().sign(&preimage(&bytes, 2));
    let statement = if insert {
        "INSERT INTO checkpoint(id,payload,signature) VALUES(1,?1,?2)"
    } else {
        "UPDATE checkpoint SET payload=?1,signature=?2 WHERE id=1"
    };
    if connection.execute(
        statement,
        rusqlite::params![bytes, signature.to_bytes().as_slice()],
    )? != 1
    {
        return Err(StoreError::Corrupt);
    }
    Ok(digest(&bytes))
}
pub(super) fn read(
    connection: &Connection,
    identity: &LocalIdentity,
    policy: &StorePolicy,
) -> Result<(Checkpoint, [u8; 32])> {
    let bytes = read_bytes(connection, identity, 2)?;
    let checkpoint: Checkpoint = serde_json::from_slice(&bytes).map_err(|_| StoreError::Corrupt)?;
    if checkpoint.version != 2 {
        return Err(StoreError::Schema);
    }
    if checkpoint.identity != identity.endpoint_id() || checkpoint.policy != policy.digest {
        return Err(StoreError::Binding);
    }
    if !checkpoint.intents.is_empty() {
        return Err(StoreError::Unsupported);
    }
    // Exact locally emitted encoding rejects ignored fields, omitted defaults and
    // alternate representations instead of silently normalizing sealed state.
    if encode(&checkpoint)? != bytes || checkpoint.revision > i64::MAX as u64 {
        return Err(StoreError::Corrupt);
    }
    Ok((checkpoint, digest(&bytes)))
}

pub(super) fn read_bytes(
    connection: &Connection,
    identity: &LocalIdentity,
    version: i64,
) -> Result<Vec<u8>> {
    schema(connection, version)?;
    let kinds: (String, String) = connection
        .query_row(
            "SELECT typeof(payload),typeof(signature) FROM checkpoint WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| StoreError::Corrupt)?;
    if kinds != ("blob".into(), "blob".into()) {
        return Err(StoreError::Corrupt);
    }
    let (count, size, signature_size): (i64, Option<i64>, Option<i64>) = connection.query_row(
        "SELECT count(*),max(length(payload)),max(length(signature)) FROM checkpoint",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if count != 1 || signature_size != Some(64) {
        return Err(StoreError::Corrupt);
    }
    if size.is_none_or(|size| size < 0 || size > MAX_CHECKPOINT_BYTES as i64) {
        return Err(StoreError::TooLarge);
    }
    let (bytes, signature): (Vec<u8>, Vec<u8>) = connection.query_row(
        "SELECT payload,signature FROM checkpoint WHERE id=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let signature: [u8; 64] = signature.try_into().map_err(|_| StoreError::Corrupt)?;
    identity
        .endpoint_id()
        .verify(
            &preimage(&bytes, version),
            &Signature::from_bytes(&signature),
        )
        .map_err(|_| StoreError::Corrupt)?;
    Ok(bytes)
}

#[cfg(test)]
thread_local! { static FORBIDDEN: std::cell::RefCell<Vec<Vec<u8>>> = const { std::cell::RefCell::new(Vec::new()) }; }
#[cfg(test)]
pub(super) struct EncodingProbe;
#[cfg(test)]
pub(super) fn forbid_encoding(values: Vec<Vec<u8>>) -> EncodingProbe {
    FORBIDDEN.with(|current| *current.borrow_mut() = values);
    EncodingProbe
}
#[cfg(test)]
impl Drop for EncodingProbe {
    fn drop(&mut self) {
        FORBIDDEN.with(|current| current.borrow_mut().clear());
    }
}
