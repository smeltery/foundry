//! The audit log read path. Entries are appended only by mutations, inside
//! their own transaction (see `Authorized::record`); there is no update or
//! delete path, and database triggers refuse both. Pages are newest first by
//! append position, which never changes, so paging is stable even when the
//! clock steps backwards.
use rusqlite::params;

use super::error::{OrgError, Storage};
use super::{Authorized, OrgStore};
use crate::authz::{Access, OperationRequest, Principal, PrincipalId, WorkspaceId};
use crate::store::database::{from_sql, to_sql};

pub const AUDIT_PAGE_DEFAULT: u32 = 50;
/// Larger page sizes are capped, not refused.
pub const AUDIT_PAGE_MAX: u32 = 200;

/// The end position of a page: pass it as [`AuditQuery::before`] to read the
/// next one. Its value is an opaque, provisional wire form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AuditCursor(u64);

impl AuditCursor {
    pub fn new(position: u64) -> Self {
        Self(position)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditQuery {
    /// Defaults to [`AUDIT_PAGE_DEFAULT`]; zero is invalid.
    pub page: Option<u32>,
    /// Exclusive upper position, from the previous page.
    pub before: Option<AuditCursor>,
    /// Exclusive upper time bound in milliseconds.
    pub until_ms: Option<u64>,
    /// Inclusive lower time bound in milliseconds.
    pub since_ms: Option<u64>,
    pub action: Option<String>,
    pub actor: Option<PrincipalId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    pub id: AuditCursor,
    pub workspace: WorkspaceId,
    pub action: String,
    /// `None` for a system action.
    pub actor: Option<PrincipalId>,
    /// The actor's email when the entry was written.
    pub actor_email: Option<String>,
    pub target_kind: String,
    /// `None` when withheld.
    pub target: Option<String>,
    pub label: Option<String>,
    pub details: Option<String>,
    /// Set when target, label and details were withheld from this caller.
    pub withheld: bool,
    pub time_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditPage {
    pub entries: Vec<AuditEntry>,
    /// Present when another page follows.
    pub next: Option<AuditCursor>,
}

impl OrgStore {
    pub fn list_audit(
        &self,
        caller: &Principal,
        workspace: &WorkspaceId,
        query: &AuditQuery,
    ) -> Result<AuditPage, OrgError> {
        let request = OperationRequest::new("audit.list").in_workspace(workspace.clone());
        self.read(caller, request, |authorized| {
            authorized.audit_page(workspace, query)
        })
    }
}

fn optional_sql(value: Option<u64>) -> Result<Option<i64>, OrgError> {
    value.map(to_sql).transpose().map_err(OrgError::from)
}

impl Authorized<'_> {
    fn audit_page(
        &self,
        workspace: &WorkspaceId,
        query: &AuditQuery,
    ) -> Result<AuditPage, OrgError> {
        let page = match query.page {
            Some(0) => return Err(OrgError::invalid("page")),
            Some(page) => page.min(AUDIT_PAGE_MAX),
            None => AUDIT_PAGE_DEFAULT,
        };
        if let (Some(since), Some(until)) = (query.since_ms, query.until_ms)
            && since > until
        {
            return Err(OrgError::invalid("since"));
        }
        let mut statement = self
            .transaction
            .prepare(
                "SELECT sequence, action, actor, actor_email, target_kind, target, label,
                        details, personal_to, created_ms
                 FROM org_audit
                 WHERE workspace = ?1
                   AND (?2 IS NULL OR sequence < ?2)
                   AND (?3 IS NULL OR created_ms < ?3)
                   AND (?4 IS NULL OR created_ms >= ?4)
                   AND (?5 IS NULL OR action = ?5)
                   AND (?6 IS NULL OR actor = ?6)
                 ORDER BY sequence DESC
                 LIMIT ?7",
            )
            .storage()?;
        let rows = statement
            .query_map(
                params![
                    workspace.as_str(),
                    optional_sql(query.before.map(AuditCursor::get))?,
                    optional_sql(query.until_ms)?,
                    optional_sql(query.since_ms)?,
                    query.action,
                    query.actor.as_ref().map(PrincipalId::as_str),
                    i64::from(page) + 1,
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, i64>(9)?,
                    ))
                },
            )
            .storage()?;
        let mut entries = Vec::new();
        for row in rows {
            let (
                sequence,
                action,
                actor,
                actor_email,
                target_kind,
                target,
                label,
                details,
                personal_to,
                created_ms,
            ) = row.storage()?;
            let withheld = self.access == Access::Partial
                && personal_to.is_some_and(|owner| owner != self.caller.id.as_str());
            entries.push(AuditEntry {
                id: AuditCursor(from_sql(sequence)?),
                workspace: workspace.clone(),
                action,
                actor: actor.map(PrincipalId::new),
                actor_email,
                target_kind,
                target: (!withheld).then_some(target),
                label: label.filter(|_| !withheld),
                details: (!withheld).then_some(details),
                withheld,
                time_ms: from_sql(created_ms)?,
            });
        }
        let more = entries.len() > page as usize;
        entries.truncate(page as usize);
        let next = more.then(|| entries.last().map(|entry| entry.id)).flatten();
        Ok(AuditPage { entries, next })
    }
}
