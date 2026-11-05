//! Single-use workspaces: separate workspaces whose parent only pays for and
//! retires them. Creation is keyed by a caller trial key per parent. The
//! decision core denies `single_use.create` for every caller until its matrix
//! cell is established; budgets and the active limit are held under C4, and
//! the restricted machine token is not issued here.
use rusqlite::{OptionalExtension, params};

use super::error::{OrgCause, OrgError, Storage};
use super::snapshot::state_name;
use super::workspace::derive_handle;
use super::{Authorized, Change, OrgStore, new_id};
use crate::authz::{OperationRequest, Principal, Role, WorkspaceId, WorkspaceState};
use crate::store::database::{from_sql, to_sql};

pub const LIFETIME_DEFAULT_MINUTES: u32 = 240;
const LIFETIME_MINUTES: std::ops::RangeInclusive<u32> = 5..=720;
const TRIAL_KEY_MAX_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleUseRequest {
    /// 1 to 200 letters, digits, `.`, `_`, `:` or `-`.
    pub trial_key: String,
    /// 5 to 720; [`LIFETIME_DEFAULT_MINUTES`] when absent.
    pub lifetime_minutes: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleUseRecord {
    pub id: WorkspaceId,
    pub parent: WorkspaceId,
    pub trial_key: String,
    pub lifetime_minutes: u32,
    pub expires_ms: u64,
    pub retired_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleUseCreated {
    pub record: SingleUseRecord,
    /// An equivalent earlier request already created it; nothing new happened.
    pub replayed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetireCause {
    Requested,
    Expired,
}

impl RetireCause {
    pub fn name(self) -> &'static str {
        match self {
            RetireCause::Requested => "requested",
            RetireCause::Expired => "expired",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        [RetireCause::Requested, RetireCause::Expired]
            .into_iter()
            .find(|cause| cause.name() == name)
    }
}

/// The saved result of a retirement. Live work and machines are owned by
/// other components, so the counts are zero and no cleanup step can fail here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetireOutcome {
    pub record: SingleUseRecord,
    pub cause: RetireCause,
    pub cancelled_runs: u32,
    pub revoked_machines: u32,
    pub failed_steps: Vec<String>,
}

fn trial_key(text: &str) -> Result<&str, OrgError> {
    let allowed = |character: char| character.is_ascii_alphanumeric() || ".:_-".contains(character);
    let valid =
        (1..=TRIAL_KEY_MAX_CHARS).contains(&text.chars().count()) && text.chars().all(allowed);
    valid.then_some(text).ok_or(OrgError::invalid("trial_key"))
}

impl OrgStore {
    pub fn create_single_use(
        &self,
        caller: &Principal,
        parent: &WorkspaceId,
        request: &SingleUseRequest,
    ) -> Result<SingleUseCreated, OrgError> {
        let operation = OperationRequest::new("single_use.create").in_workspace(parent.clone());
        self.write(caller, operation, |authorized| {
            authorized.create_single_use(parent, request)
        })
    }

    /// Retires a single-use workspace of `parent` and revokes its credentials.
    /// Repeating it returns the saved result.
    pub fn retire_single_use(
        &self,
        caller: &Principal,
        parent: &WorkspaceId,
        single_use: &WorkspaceId,
    ) -> Result<RetireOutcome, OrgError> {
        let operation = OperationRequest::new("single_use.retire")
            .in_workspace(parent.clone())
            .retiring(single_use.clone());
        self.write(caller, operation, |authorized| {
            authorized.retire_single_use(parent, single_use)
        })
    }
}

impl Authorized<'_> {
    pub(super) fn create_single_use(
        &self,
        parent: &WorkspaceId,
        request: &SingleUseRequest,
    ) -> Result<SingleUseCreated, OrgError> {
        let key = trial_key(&request.trial_key)?;
        let lifetime = request.lifetime_minutes.unwrap_or(LIFETIME_DEFAULT_MINUTES);
        if !LIFETIME_MINUTES.contains(&lifetime) {
            return Err(OrgError::invalid("lifetime_minutes"));
        }
        if let Some((record, _)) = self.single_use(parent, Lookup::TrialKey(key))? {
            let cause = if record.retired_ms.is_some() || record.expires_ms <= self.now_ms {
                OrgCause::WorkspaceRetired
            } else if record.lifetime_minutes != lifetime {
                OrgCause::KeyReused
            } else {
                return Ok(SingleUseCreated {
                    record,
                    replayed: true,
                });
            };
            return Err(OrgError::Conflict { cause });
        }
        let id = WorkspaceId::new(new_id("su")?);
        let now = to_sql(self.now_ms)?;
        let expires_ms = self.now_ms.saturating_add(u64::from(lifetime) * 60_000);
        self.transaction
            .execute(
                "INSERT INTO org_workspaces (id, name, handle, state, created_ms, parent,
                     trial_key, lifetime_minutes, expires_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id.as_str(),
                    key,
                    derive_handle(key),
                    state_name(WorkspaceState::Active),
                    now,
                    parent.as_str(),
                    key,
                    lifetime,
                    to_sql(expires_ms)?,
                ],
            )
            .storage()?;
        self.transaction
            .execute(
                "INSERT INTO org_members (workspace, principal, role, joined_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    id.as_str(),
                    self.caller.id.as_str(),
                    Role::Owner.name(),
                    now
                ],
            )
            .storage()?;
        self.record(
            parent,
            Change {
                action: "single_use.create",
                target_kind: "workspace",
                target: id.as_str(),
                label: Some(key),
                details: format!("lifetime_minutes={lifetime}"),
            },
        )?;
        Ok(SingleUseCreated {
            record: SingleUseRecord {
                id,
                parent: parent.clone(),
                trial_key: key.to_owned(),
                lifetime_minutes: lifetime,
                expires_ms,
                retired_ms: None,
            },
            replayed: false,
        })
    }

    fn retire_single_use(
        &self,
        parent: &WorkspaceId,
        single_use: &WorkspaceId,
    ) -> Result<RetireOutcome, OrgError> {
        let (mut record, saved) =
            self.single_use(parent, Lookup::Id(single_use))?
                .ok_or(OrgError::NotFound {
                    what: "single_use",
                    cause: None,
                })?;
        let outcome = |record, cause| RetireOutcome {
            record,
            cause,
            cancelled_runs: 0,
            revoked_machines: 0,
            failed_steps: Vec::new(),
        };
        if let Some(cause) = saved {
            return Ok(outcome(record, cause));
        }
        let cause = if record.expires_ms <= self.now_ms {
            RetireCause::Expired
        } else {
            RetireCause::Requested
        };
        let now = to_sql(self.now_ms)?;
        self.transaction
            .execute(
                "UPDATE org_workspaces SET state = ?2, retired_ms = ?3, retire_cause = ?4
                 WHERE id = ?1",
                params![
                    single_use.as_str(),
                    state_name(WorkspaceState::Retired),
                    now,
                    cause.name(),
                ],
            )
            .storage()?;
        self.transaction
            .execute(
                "UPDATE credentials SET revoked_ms = ?2 WHERE workspace = ?1 AND revoked_ms IS NULL",
                params![single_use.as_str(), now],
            )
            .storage()?;
        self.record(
            parent,
            Change {
                action: "single_use.retire",
                target_kind: "workspace",
                target: single_use.as_str(),
                label: Some(&record.trial_key),
                details: format!("cause={}", cause.name()),
            },
        )?;
        record.retired_ms = Some(self.now_ms);
        Ok(outcome(record, cause))
    }

    /// The single-use workspace of `parent` and its saved retirement cause.
    fn single_use(
        &self,
        parent: &WorkspaceId,
        lookup: Lookup<'_>,
    ) -> Result<Option<(SingleUseRecord, Option<RetireCause>)>, OrgError> {
        let (column, value) = match lookup {
            Lookup::Id(id) => ("id", id.as_str()),
            Lookup::TrialKey(key) => ("trial_key", key),
        };
        let row = self
            .transaction
            .query_row(
                &format!(
                    "SELECT id, trial_key, lifetime_minutes, expires_ms, retired_ms, retire_cause
                     FROM org_workspaces WHERE parent = ?1 AND {column} = ?2"
                ),
                params![parent.as_str(), value],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, u32>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()
            .storage()?;
        let Some((id, trial_key, lifetime_minutes, expires_ms, retired_ms, cause)) = row else {
            return Ok(None);
        };
        let cause = cause
            .map(|name| {
                RetireCause::from_name(&name)
                    .ok_or(OrgError::unavailable("malformed stored retirement cause"))
            })
            .transpose()?;
        let record = SingleUseRecord {
            id: WorkspaceId::new(id),
            parent: parent.clone(),
            trial_key,
            lifetime_minutes,
            expires_ms: from_sql(expires_ms)?,
            retired_ms: retired_ms.map(from_sql).transpose()?,
        };
        Ok(Some((record, cause)))
    }
}

/// How a single-use workspace is found. The column name is a fixed literal,
/// never caller text.
#[derive(Clone, Copy)]
enum Lookup<'a> {
    Id(&'a WorkspaceId),
    TrialKey(&'a str),
}
