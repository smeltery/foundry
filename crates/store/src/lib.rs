//! Durable sessions with transactional writes and expiring, fenced attempt leases.
mod admission;
mod attempts;
mod capture_bounds;
mod capture_snapshot;
mod checkpoint_codec;
mod checkpoint_cut;
mod checkpoint_finish;
mod checkpoint_read;
mod checkpoint_types;
mod checkpoints;
mod events;
mod fresh_attempt;
mod messages;
mod restore_continue;
mod restore_message;
mod restore_read;
mod restore_types;
mod restores;
mod schema;
mod tools;
pub mod witness;
mod witness_binding;
mod witness_events;
mod witness_facts;
mod witness_storage;
mod witnessed_checkpoint;
mod work;

use foundry_contracts::{ContractError, CreateSession, Lifecycle, Session};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{
    fmt,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub use attempts::AttemptFence;
pub use capture_snapshot::{
    CaptureMessageEvidence, CaptureSnapshot, CaptureToolEvidence, MAX_CAPTURE_EVIDENCE_ENTRIES,
};
pub use checkpoint_finish::{FinalizedCheckpoint, TerminalProposal};
pub use checkpoint_types::{
    CheckpointCut, CheckpointRef, ContextCheckpointId, ContextFormat, CoverageManifest,
    MAX_CHECKPOINT_BYTES, MAX_CHECKPOINT_COVERAGE_BYTES, MAX_CHECKPOINT_METADATA_BYTES,
    OpaqueCheckpointInput, PrivateCheckpoint,
};
pub use fresh_attempt::FreshAttempt;
pub use restore_types::{
    BeginRestoreInput, BeginRestoreResult, ContinueDecision, ContinueInput, ContinueMessageBinding,
    ContinueMessageDecision, ContinueMessageInput, ContinueMessagePermit, ContinuePermit,
    ContinueReceipt, RestoreClose, RestoreFence, RestorePhase, RestoreRef,
};
pub use restores::{
    MAX_RESTORE_ROWS_PER_DATABASE, MAX_RESTORE_ROWS_PER_SESSION, RESTORE_RECOVERY_BATCH_SIZE,
};
pub use tools::{ToolOutcome, ToolRecord};
pub use witnessed_checkpoint::{PrivateWitness, WitnessedCheckpointInput};

#[derive(Debug)]
pub enum StoreError {
    Database(rusqlite::Error),
    Json(serde_json::Error),
    Contract(ContractError),
    Clock,
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(e) => write!(f, "database: {e}"),
            Self::Json(e) => write!(f, "stored data: {e}"),
            Self::Contract(e) => e.fmt(f),
            Self::Clock => write!(f, "system clock is outside the supported range"),
        }
    }
}
impl std::error::Error for StoreError {}
impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database(e)
    }
}
impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
impl From<ContractError> for StoreError {
    fn from(e: ContractError) -> Self {
        Self::Contract(e)
    }
}
pub type Result<T> = std::result::Result<T, StoreError>;

pub struct Store {
    connection: Connection,
    lease_duration: Duration,
}
impl Store {
    /// Opening a database never recovers or cancels existing attempts.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_lease(path, Duration::from_secs(60))
    }

    /// Owners must renew before this duration elapses, including during long tool calls.
    pub fn open_with_lease(path: impl AsRef<Path>, lease_duration: Duration) -> Result<Self> {
        if lease_duration.as_millis() == 0 || lease_duration.as_millis() > i64::MAX as u128 {
            return Err(ContractError::Invalid(
                "lease duration must fit positive milliseconds".into(),
            )
            .into());
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )?;
        schema::migrate(&mut connection)?;
        Ok(Self {
            connection,
            lease_duration,
        })
    }

    pub fn create(&mut self, request: CreateSession) -> Result<Session> {
        request.validate()?;
        let encoded = serde_json::to_string(&request)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(key) = &request.idempotency_key {
            let existing: Option<(String, String)> = tx
                .query_row(
                    "SELECT id, creation FROM sessions WHERE idempotency_key=?1",
                    [key],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((id, creation)) = existing {
                if creation != encoded {
                    return Err(conflict("idempotency key has a different request"));
                }
                return read_session(&tx, &id);
            }
        }
        let session = Session {
            id: Uuid::new_v4().to_string(),
            prompt: request.prompt,
            directory: request.directory,
            model: request.model,
            lifecycle: Lifecycle::Queued,
            outcome: None,
            result: None,
            active_attempt_id: None,
            version: 0,
        };
        tx.execute(
            "INSERT INTO sessions(id,idempotency_key,creation,body) VALUES(?1,?2,?3,?4)",
            params![
                session.id,
                request.idempotency_key,
                encoded,
                serde_json::to_string(&session)?
            ],
        )?;
        tx.execute(
            "INSERT INTO session_work(session_id,revision) VALUES(?1,0)",
            [&session.id],
        )?;
        tx.commit()?;
        Ok(session)
    }

    pub fn get(&self, session_id: &str) -> Result<Session> {
        read_session(&self.connection, session_id)
    }

    /// Newest created sessions first; updates do not reorder the list.
    pub fn list(&self, limit: u32) -> Result<Vec<Session>> {
        let mut statement = self
            .connection
            .prepare("SELECT body FROM sessions ORDER BY rowid DESC LIMIT ?1")?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
}

fn read_session(connection: &Connection, id: &str) -> Result<Session> {
    let body: Option<String> = connection
        .query_row("SELECT body FROM sessions WHERE id=?1", [id], |r| r.get(0))
        .optional()?;
    let body =
        body.ok_or_else(|| StoreError::Contract(ContractError::NotFound("session".into())))?;
    Ok(serde_json::from_str(&body)?)
}
fn save_session(connection: &Connection, session: &mut Session) -> Result<()> {
    session.version = session
        .version
        .checked_add(1)
        .ok_or_else(|| conflict("session version exhausted"))?;
    connection.execute(
        "UPDATE sessions SET body=?1 WHERE id=?2",
        params![serde_json::to_string(session)?, session.id],
    )?;
    Ok(())
}
fn conflict(message: &str) -> StoreError {
    ContractError::Conflict(message.into()).into()
}
fn now_ms() -> Result<i64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::Clock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| StoreError::Clock)
}
