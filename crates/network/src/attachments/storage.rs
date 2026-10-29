use super::*;
use crate::persistence::storage::{OpenMode, Protection};
use cap_std::{ambient_authority, fs::Dir};
use rusqlite::{Connection, TransactionBehavior, params};
use std::time::Duration;

/// Payload and quota reservations survive materialization failure. The caller
/// must release only after every pending/unread/held reference has ended.
pub struct AttachmentStore {
    pub(super) connection: Connection,
    pub(super) protection: Protection,
    pub(super) lock: Protection,
    pub(super) root: Dir,
    path: PathBuf,
    frames: Vec<String>,
}
impl AttachmentStore {
    fn save_names(&self, id: &str, names: &[String]) -> Result<()> {
        let bytes = serde_json::to_vec(names).map_err(|_| AttachmentError::Corrupt)?;
        self.connection.execute(
            "UPDATE batches SET names=?2 WHERE id=?1",
            params![id, bytes],
        )?;
        self.protection.sync()?;
        Ok(())
    }
    /// Receiver root must already exist, absolute, privately owned and mode 0700.
    pub fn open(receiver_root: &Path) -> Result<Self> {
        Self::open_with_frames(receiver_root, &[])
    }
    pub fn open_with_frames(receiver_root: &Path, words: &[String]) -> Result<Self> {
        frame_names::validate(words)?;
        if !receiver_root.is_absolute() {
            return Err(AttachmentError::Invalid);
        }
        let protection =
            Protection::open_named(receiver_root, "attachments.sqlite3", OpenMode::CreateOrOpen)?;
        let lock =
            Protection::open_named(receiver_root, "attachments.lock", OpenMode::CreateOrOpen)?;
        let guard = lock.lock(Duration::from_secs(5))?;
        let mut connection = Connection::open_with_flags(
            protection.path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA trusted_schema=OFF; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;",
        )?;
        database::initialize(&mut connection)?;
        connection.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA max_page_count=65536;")?;
        protection.sync()?;
        let path = receiver_root.canonicalize()?;
        let root = Dir::open_ambient_dir(&path, ambient_authority())?;
        drop(guard);
        Ok(Self {
            connection,
            protection,
            lock,
            root,
            path,
            frames: words.to_vec(),
        })
    }
    /// Independent ingress validation precedes durable reservation and any file creation.
    /// Exact retries preserve the original admission time and do not charge twice.
    pub fn persist(
        &mut self,
        key: &AttachmentKey,
        files: &[TextAttachment],
        now_ms: i64,
    ) -> Result<Vec<TextAttachment>> {
        let id = key.directory()?;
        let files = names::normalize_with(files, &self.frames)?;
        let bytes = validate_received(&files)?;
        let payload = serde_json::to_vec(&files).map_err(|_| AttachmentError::Invalid)?;
        let fingerprint = blake3::hash(&payload);
        if now_ms < 0 {
            return Err(AttachmentError::Invalid);
        }
        let _guard = self.lock.lock(Duration::from_secs(5))?;
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let fixed = if let Some(prior) = database::read(&tx, &id)? {
            if prior.key != *key || prior.fingerprint != fingerprint.as_bytes() {
                return Err(AttachmentError::Conflict);
            }
            if prior.state >= 3 {
                return Err(AttachmentError::Expired);
            }
            if prior.state == 2 {
                return Err(AttachmentError::Deleting);
            }
            prior.names
        } else {
            database::quota(&tx, &key.sender, bytes, now_ms)?;
            tx.execute("INSERT INTO batches(id,sender,recipient,message,payload,bytes,accepted,state,fingerprint,names) VALUES(?1,?2,?3,?4,?5,?6,?7,0,?8,?9)", params![id,key.sender,key.recipient,key.message,payload,bytes as i64,now_ms,fingerprint.as_bytes().as_slice(),b"[]".as_slice()])?;
            Vec::new()
        };
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        let (located, names) = materialize::files(&self.root, &self.path, key, &files, &fixed)?;
        self.save_names(&id, &names)?;
        self.protection.check()?;
        Ok(located)
    }
    /// Restores interrupted owned files from the immutable durable payload.
    /// Conflicting bytes, hardlinks or symlinks fail without overwrite.
    pub fn recover(&mut self, key: &AttachmentKey) -> Result<Vec<TextAttachment>> {
        let id = key.directory()?;
        let _guard = self.lock.lock(Duration::from_secs(5))?;
        self.protection.check()?;
        let batch = database::read(&self.connection, &id)?.ok_or(AttachmentError::Missing)?;
        if batch.state >= 3 {
            return Err(AttachmentError::Expired);
        }
        if batch.state == 2 {
            return Err(AttachmentError::Deleting);
        }
        let (files, names) =
            materialize::files(&self.root, &self.path, key, &batch.files, &batch.names)?;
        self.save_names(&id, &names)?;
        self.protection.check()?;
        Ok(files)
    }
    /// Privileged outbox/retry material, independently validated and without local paths.
    /// Does not claim delivery or recreate files; callers still authorize this owner key.
    pub fn load_payload(&mut self, key: &AttachmentKey) -> Result<Vec<TextAttachment>> {
        let id = key.directory()?;
        let _guard = self.lock.lock(Duration::from_secs(5))?;
        self.protection.check()?;
        let batch = database::read(&self.connection, &id)?.ok_or(AttachmentError::Missing)?;
        if batch.state >= 3 {
            return Err(AttachmentError::Expired);
        }
        if batch.state == 2 {
            return Err(AttachmentError::Deleting);
        }
        Ok(batch.files)
    }
    /// Trusted host assertion that no pending, held or unread reference remains.
    pub fn release(&mut self, key: &AttachmentKey, now_ms: i64) -> Result<()> {
        let id = key.directory()?;
        let _guard = self.lock.lock(Duration::from_secs(5))?;
        self.protection.check()?;
        let batch = database::read(&self.connection, &id)?.ok_or(AttachmentError::Missing)?;
        if now_ms < batch.accepted {
            return Err(AttachmentError::Invalid);
        }
        if batch.state >= 3 {
            return Err(AttachmentError::Expired);
        }
        if batch.state == 2 {
            return Err(AttachmentError::Deleting);
        }
        self.connection.execute(
            "UPDATE batches SET state=1,released=coalesce(released,?2) WHERE id=?1",
            params![id, now_ms],
        )?;
        self.protection.sync()?;
        Ok(())
    }
    /// Explicit per-owner cleanup only after release and one day's retention.
    /// Persisted deletion state prevents retry/recovery from recreating removed files.
    /// Removing the record ends this key's deduplication lifetime.
    pub fn cleanup(&mut self, key: &AttachmentKey, now_ms: i64) -> Result<()> {
        let id = key.directory()?;
        let _guard = self.lock.lock(Duration::from_secs(5))?;
        self.protection.check()?;
        let batch = database::read(&self.connection, &id)?.ok_or(AttachmentError::Missing)?;
        if batch.state >= 3 {
            return Err(AttachmentError::Expired);
        }
        let released = batch.released.ok_or(AttachmentError::InUse)?;
        if now_ms < 0 || now_ms.saturating_sub(released) < RETAIN_RELEASED_MS {
            return Err(AttachmentError::Retention);
        }
        self.connection
            .execute("UPDATE batches SET state=2 WHERE id=?1", [&id])?;
        self.protection.sync()?;
        materialize::delete(&self.root, key, &batch.files, &batch.names)?;
        self.protection.check()?;
        self.connection
            .execute("DELETE FROM batches WHERE id=?1 AND state=2", [&id])?;
        self.protection.sync()?;
        Ok(())
    }
    pub fn usage(&self) -> Result<Usage> {
        self.protection.check()?;
        self.connection
            .query_row(
                "SELECT count(*),coalesce(sum(bytes),0) FROM batches",
                [],
                |r| {
                    Ok(Usage {
                        batches: r.get(0)?,
                        bytes: r.get(1)?,
                    })
                },
            )
            .map_err(Into::into)
    }
}
