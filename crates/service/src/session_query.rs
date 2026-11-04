//! Session list requests: page-size defaults and caps, sparse fields and the
//! page validator (`docs/architecture/session-api.md`, "List sessions"). Field
//! names are provisional pending the naming decision.
use crate::{Result, Service};
use foundry_contracts::{ContractError, Session};
pub use foundry_store::{ArchiveFilter, OriginFilter};
use foundry_store::{
    DEFAULT_SESSION_PAGE, MAX_SEARCH_PAGE, MAX_SESSION_PAGE, SessionFilter, StoreError,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Session fields a caller may select. `id` is always returned.
pub const SESSION_FIELDS: [&str; 16] = [
    "id",
    "prompt",
    "directory",
    "model",
    "lifecycle",
    "outcome",
    "result",
    "active_attempt_id",
    "version",
    "archived",
    "title",
    "origin",
    "created_ms",
    "updated_ms",
    "parent_session_id",
    "forked_from_session_id",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionListRequest {
    pub archive: ArchiveFilter,
    pub origins: Vec<OriginFilter>,
    pub search: Option<String>,
    pub after: Option<String>,
    /// Primary page size; wins over `limit` when both are present.
    pub page_size: Option<u32>,
    /// Older alias of `page_size`.
    pub limit: Option<u32>,
    /// Empty selects every field.
    pub fields: Vec<String>,
    pub if_none_match: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionList {
    Page(SessionListPage),
    /// The caller's validator still matches the page.
    NotModified {
        etag: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionListPage {
    pub sessions: Vec<Map<String, Value>>,
    pub next_cursor: Option<String>,
    /// Page size after defaults and caps.
    pub effective_page: u32,
    /// Weak validator over the query and every visible byte of the page.
    pub etag: String,
}

impl SessionListRequest {
    /// Zero is rejected; larger values are capped, at 24 for a title search.
    pub fn effective_page(&self) -> Result<u32> {
        let requested = self
            .page_size
            .or(self.limit)
            .unwrap_or(DEFAULT_SESSION_PAGE);
        if requested == 0 {
            return Err(invalid("session page size must be positive"));
        }
        let cap = if self.search.is_some() {
            MAX_SEARCH_PAGE
        } else {
            MAX_SESSION_PAGE
        };
        Ok(requested.min(cap))
    }

    fn selected_fields(&self) -> Result<Option<Vec<&str>>> {
        if self.fields.is_empty() {
            return Ok(None);
        }
        let mut selected = vec!["id"];
        for name in &self.fields {
            let Some(&known) = SESSION_FIELDS.iter().find(|known| **known == name) else {
                return Err(invalid("unknown session field"));
            };
            if !selected.contains(&known) {
                selected.push(known);
            }
        }
        Ok(Some(selected))
    }
}

impl Service {
    pub fn list_sessions(&self, request: &SessionListRequest) -> Result<SessionList> {
        let page = request.effective_page()?;
        let fields = request.selected_fields()?;
        let filter = SessionFilter {
            archive: request.archive,
            origins: request.origins.clone(),
            title_search: request.search.clone(),
        };
        let found = self
            .store
            .query_sessions(&filter, request.after.as_deref(), page)?;
        let sessions = found
            .sessions
            .iter()
            .map(|session| project(session, fields.as_deref()))
            .collect::<Result<Vec<_>>>()?;
        let etag = page_etag(
            &filter,
            request,
            page,
            fields.as_deref(),
            &sessions,
            &found.next_cursor,
        )?;
        if request.if_none_match.as_deref() == Some(etag.as_str()) {
            return Ok(SessionList::NotModified { etag });
        }
        Ok(SessionList::Page(SessionListPage {
            sessions,
            next_cursor: found.next_cursor,
            effective_page: page,
            etag,
        }))
    }
}

/// A selected field the session does not store is null, except `archived`.
fn project(session: &Session, fields: Option<&[&str]>) -> Result<Map<String, Value>> {
    let Value::Object(full) = serde_json::to_value(session).map_err(StoreError::Json)? else {
        return Err(invalid("session does not encode as an object"));
    };
    let Some(fields) = fields else {
        return Ok(full);
    };
    Ok(fields
        .iter()
        .map(|&name| {
            let value = full.get(name).cloned().unwrap_or(match name {
                "archived" => Value::Bool(false),
                _ => Value::Null,
            });
            (name.to_owned(), value)
        })
        .collect())
}

/// Stable while the query and the selected, visible page are unchanged.
fn page_etag(
    filter: &SessionFilter,
    request: &SessionListRequest,
    page: u32,
    fields: Option<&[&str]>,
    sessions: &[Map<String, Value>],
    next_cursor: &Option<String>,
) -> Result<String> {
    let mut hash = Sha256::new();
    let mut field = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    };
    field(b"foundry.session-list.page.v1");
    field(&filter.digest());
    field(request.after.as_deref().unwrap_or("").as_bytes());
    field(&page.to_be_bytes());
    field(
        fields
            .map_or(String::new(), |names| names.join(","))
            .as_bytes(),
    );
    for session in sessions {
        field(&serde_json::to_vec(session).map_err(StoreError::Json)?);
    }
    field(next_cursor.as_deref().unwrap_or("").as_bytes());
    let digest = hash.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("W/\"{hex}\""))
}

fn invalid(message: &str) -> crate::ServiceError {
    StoreError::Contract(ContractError::Invalid(message.into())).into()
}
