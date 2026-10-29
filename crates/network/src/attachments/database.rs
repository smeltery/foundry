use super::*;
use rusqlite::{Connection, OptionalExtension};
pub(super) const TABLE: &str = "CREATE TABLE batches (id TEXT PRIMARY KEY,sender TEXT NOT NULL,recipient TEXT NOT NULL,message TEXT NOT NULL,payload BLOB NOT NULL,bytes INTEGER NOT NULL CHECK(bytes>=0),accepted INTEGER NOT NULL CHECK(accepted>=0),released INTEGER,state INTEGER NOT NULL CHECK(state IN (0,1,2,3,4)),fingerprint BLOB NOT NULL,names BLOB NOT NULL,expiry INTEGER)";
pub(super) struct Batch {
    pub key: AttachmentKey,
    pub files: Vec<TextAttachment>,
    pub accepted: i64,
    pub released: Option<i64>,
    pub state: i64,
    pub fingerprint: Vec<u8>,
    pub names: Vec<String>,
    pub expiry: Option<i64>,
}
pub(super) fn initialize(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let app: i64 = tx.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    if app == 0 && version == 0 && count == 0 {
        tx.execute_batch(TABLE)?;
        tx.pragma_update(None, "application_id", 0x46354154)?;
        tx.pragma_update(None, "user_version", 2)?;
    } else {
        let sql: String = tx.query_row(
            "SELECT sql FROM sqlite_schema WHERE name='batches' AND type='table'",
            [],
            |r| r.get(0),
        )?;
        if app != 0x46354154 || version != 2 || count != 1 || sql != TABLE {
            return Err(AttachmentError::Corrupt);
        }
    }
    tx.commit()?;
    Ok(())
}
pub(super) fn read(connection: &Connection, id: &str) -> Result<Option<Batch>> {
    let raw = connection.query_row(
        "SELECT sender,recipient,message,payload,bytes,accepted,released,state,fingerprint,names,expiry FROM batches WHERE id=?1 AND length(payload)<=1056768 AND length(names)<=4096 AND length(fingerprint)=32",
        [id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Vec<u8>>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?, r.get::<_, Option<i64>>(6)?, r.get::<_, i64>(7)?, r.get::<_, Vec<u8>>(8)?, r.get::<_, Vec<u8>>(9)?, r.get::<_, Option<i64>>(10)?)),
    ).optional()?;
    let Some((
        sender,
        recipient,
        message,
        payload,
        bytes,
        accepted,
        released,
        state,
        fingerprint,
        names,
        expiry,
    )) = raw
    else {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM batches WHERE id=?1)",
            [id],
            |r| r.get(0),
        )?;
        return if exists {
            Err(AttachmentError::Corrupt)
        } else {
            Ok(None)
        };
    };
    let key =
        AttachmentKey::new(&sender, &recipient, &message).map_err(|_| AttachmentError::Corrupt)?;
    let files: Vec<TextAttachment> =
        serde_json::from_slice(&payload).map_err(|_| AttachmentError::Corrupt)?;
    let actual = validate_received(&files).map_err(|_| AttachmentError::Corrupt)?;
    let normalized = normalize_received(&files).map_err(|_| AttachmentError::Corrupt)?;
    let names: Vec<String> =
        serde_json::from_slice(&names).map_err(|_| AttachmentError::Corrupt)?;
    let unique: std::collections::BTreeSet<_> = names.iter().map(|n| n.to_lowercase()).collect();
    if unique.len() != names.len() || (state < 3 && expiry.is_some()) {
        return Err(AttachmentError::Corrupt);
    }
    if normalized != files
        || fingerprint.len() != 32
        || !matches!(state, 0..=4)
        || (!names.is_empty() && names.len() != files.len())
        || names.iter().any(|n| !super::names::component(n))
        || (state != 3 && fingerprint.as_slice() != blake3::hash(&payload).as_bytes())
        || (state == 3 && (!files.is_empty() || !names.is_empty()))
        || (state >= 3 && !matches!(expiry, Some(0..=2)))
    {
        return Err(AttachmentError::Corrupt);
    }
    if key.directory()? != id
        || actual as i64 != bytes
        || accepted < 0
        || !matches!((state, released), (0, None) | (1 | 2, Some(_)) | (3 | 4, _))
        || released.is_some_and(|r| r < accepted)
    {
        return Err(AttachmentError::Corrupt);
    }
    Ok(Some(Batch {
        key,
        files,
        accepted,
        released,
        state,
        fingerprint,
        names,
        expiry,
    }))
}
pub(super) fn quota(connection: &Connection, sender: &str, bytes: usize, now: i64) -> Result<()> {
    let (count, total, kept, recent, latest): (u64, u64, u64, u64, i64) = connection.query_row(
        "SELECT count(*),coalesce(sum(bytes),0),coalesce(sum(CASE WHEN sender=?1 THEN bytes ELSE 0 END),0),coalesce(sum(CASE WHEN sender=?1 AND accepted>=?2 THEN bytes ELSE 0 END),0),coalesce(max(accepted),0) FROM batches",
        rusqlite::params![sender, now.saturating_sub(3_600_000)],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    )?;
    if now < latest {
        return Err(AttachmentError::Invalid);
    }
    if count >= MAX_BATCHES
        || total + bytes as u64 > MAX_KEPT_BYTES
        || kept + bytes as u64 > MAX_SENDER_BYTES
        || recent + bytes as u64 > MAX_SENDER_HOURLY_BYTES
    {
        return Err(AttachmentError::Quota);
    }
    Ok(())
}
