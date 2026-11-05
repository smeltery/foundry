//! Session create with contract v2 options. Every check runs before the write
//! transaction. The stored `creation` string is the complete request and the
//! idempotency comparison; absent options are omitted from it, so a request
//! without v2 options keeps the bytes an older store recorded.
use crate::{Result, Store, conflict, read_session};
use foundry_contracts::{
    ContractError, CreateSession, Lifecycle, Session, SessionMetadata, SessionOrigin, TargetKind,
    WorkspaceSelector,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

/// Associations recorded at create. Tags keep their request order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAssociations {
    /// Absent from the request selects a repository-neutral scratch start.
    pub workspace: WorkspaceSelector,
    pub tags: Vec<String>,
    pub tool_profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreateReceipt {
    pub session: Session,
    pub associations: SessionAssociations,
    /// The key matched an equal earlier request; nothing new was written.
    pub replayed: bool,
}

impl Store {
    /// Validates, then records a queued session with its metadata and
    /// associations, or replays the session an equal keyed request created.
    pub fn create_session(&mut self, request: CreateSession) -> Result<CreateReceipt> {
        request.validate()?;
        if let Some(profile) = &request.tool_profile {
            check_reference("tool profile", profile)?;
        }
        check_available(&request)?;
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
                return Ok(CreateReceipt {
                    session: read_session(&tx, &id)?,
                    associations: read_associations(&tx, &id)?,
                    replayed: true,
                });
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
            metadata: Box::new(SessionMetadata {
                title: request.title.clone(),
                origin: request.origin,
                ..Default::default()
            }),
        };
        let (origin_kind, origin_interface) = origin_columns(session.metadata.origin.as_ref());
        tx.execute(
            "INSERT INTO sessions(id,idempotency_key,creation,body,title,origin_kind,origin_interface)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                session.id,
                request.idempotency_key,
                encoded,
                serde_json::to_string(&session)?,
                request.title,
                origin_kind,
                origin_interface
            ],
        )?;
        tx.execute(
            "INSERT INTO session_work(session_id,revision) VALUES(?1,0)",
            [&session.id],
        )?;
        for (position, tag) in request.tags.iter().enumerate() {
            tx.execute(
                "INSERT INTO session_tags(session_id,tag,position) VALUES(?1,?2,?3)",
                params![session.id, tag, position as i64],
            )?;
        }
        tx.commit()?;
        Ok(CreateReceipt {
            session,
            associations: SessionAssociations {
                workspace: request.workspace.unwrap_or(WorkspaceSelector::Scratch {}),
                tags: request.tags,
                tool_profile: request.tool_profile,
            },
            replayed: false,
        })
    }

    pub fn associations(&self, session_id: &str) -> Result<SessionAssociations> {
        read_associations(&self.connection, session_id)
    }
}

/// Options that validate but cannot be honored fail before any write and never
/// fall back to local execution.
fn check_available(request: &CreateSession) -> Result<()> {
    if let Some(target) = &request.target {
        match target.kind {
            TargetKind::Machine => {
                return Err(ContractError::Unsupported("machine targets".into()).into());
            }
        }
    }
    // Inline criteria are stored in the creation record and admitted per attempt
    // (completion-policy.md). No configuration profile source exists yet.
    if request
        .completion
        .as_ref()
        .is_some_and(|criteria| criteria.profile.is_some())
    {
        return Err(ContractError::Unsupported("completion criteria profile".into()).into());
    }
    Ok(())
}

/// A reference is `/`-separated segments of lowercase ASCII letters, digits,
/// `-`, `_` and `.`, with no empty, `.` or `..` segment. The contract has
/// already bounded its length.
fn check_reference(field: &str, value: &str) -> Result<()> {
    let segment_ok = |segment: &str| {
        !matches!(segment, "" | "." | "..")
            && segment
                .bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.'))
    };
    if !value.split('/').all(segment_ok) {
        return Err(ContractError::Invalid(format!(
            "{field} must be lowercase segments of letters, digits, '-', '_' or '.'"
        ))
        .into());
    }
    Ok(())
}

fn origin_columns(origin: Option<&SessionOrigin>) -> (Option<&'static str>, Option<&str>) {
    match origin {
        None => (None, None),
        Some(SessionOrigin::Interactive {}) => (Some("interactive"), None),
        Some(SessionOrigin::External { interface }) => (Some("external"), interface.as_deref()),
    }
}

/// Workspace and tool profile come from the stored request; tags come from
/// `session_tags`, which later tag operations own.
fn read_associations(connection: &Connection, session_id: &str) -> Result<SessionAssociations> {
    let creation: Option<String> = connection
        .query_row(
            "SELECT creation FROM sessions WHERE id=?1 AND deleted_ms IS NULL",
            [session_id],
            |row| row.get(0),
        )
        .optional()?;
    let creation = creation.ok_or_else(|| ContractError::NotFound("session".into()))?;
    let request: CreateSession = serde_json::from_str(&creation)?;
    let mut statement =
        connection.prepare("SELECT tag FROM session_tags WHERE session_id=?1 ORDER BY position")?;
    let tags = statement
        .query_map([session_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(SessionAssociations {
        workspace: request.workspace.unwrap_or(WorkspaceSelector::Scratch {}),
        tags,
        tool_profile: request.tool_profile,
    })
}
