//! Filtered, cursor-paged session listing (`docs/architecture/session-api.md`,
//! "List sessions"). Filters read the v12 metadata columns; rows are the stored
//! session bodies. Order is newest created first, and the cursor names the last
//! returned session, so sessions created while a reader pages never shift or
//! repeat later pages. Wire names and the cursor encoding are provisional.
use crate::{Result, Store, checkpoint_cut::digest};
use foundry_contracts::{ContractError, Session};
use rusqlite::{OptionalExtension, params_from_iter, types::Value};

pub const DEFAULT_SESSION_PAGE: u32 = 50;
pub const MAX_SESSION_PAGE: u32 = 200;
pub const MAX_SEARCH_PAGE: u32 = 24;
pub const MIN_SEARCH_CHARS: usize = 3;
pub const MAX_SEARCH_CHARS: usize = 500;
/// A session without a stored title is searched by this many prompt characters.
pub const DERIVED_TITLE_CHARS: usize = 200;
const MAX_INTERFACE_BYTES: usize = 64;
const CURSOR_PREFIX: &str = "sl1";

/// Archive state, not an execution status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ArchiveFilter {
    #[default]
    Active,
    Archived,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum OriginFilter {
    Interactive,
    /// Any external interface.
    External,
    ExternalInterface(String),
}

/// Filters shared by every page of one listing; a cursor is bound to them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionFilter {
    pub archive: ArchiveFilter,
    /// Empty means every origin, including sessions written before origins existed.
    pub origins: Vec<OriginFilter>,
    /// Literal, case-insensitive substring of the stored or derived title.
    pub title_search: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionPage {
    pub sessions: Vec<Session>,
    /// Present only when at least one more matching session exists.
    pub next_cursor: Option<String>,
}

impl SessionFilter {
    fn validate(&self, page: u32) -> Result<()> {
        let cap = match &self.title_search {
            Some(text) => {
                let chars = text.chars().count();
                if !(MIN_SEARCH_CHARS..=MAX_SEARCH_CHARS).contains(&chars) {
                    return Err(invalid("title search must contain 3 to 500 characters"));
                }
                MAX_SEARCH_PAGE
            }
            None => MAX_SESSION_PAGE,
        };
        if !(1..=cap).contains(&page) {
            return Err(invalid("session page size is outside its bounds"));
        }
        for origin in &self.origins {
            if let OriginFilter::ExternalInterface(name) = origin
                && (name.trim().is_empty() || name.len() > MAX_INTERFACE_BYTES)
            {
                return Err(invalid("origin interface must contain 1 to 64 UTF-8 bytes"));
            }
        }
        Ok(())
    }

    /// Order-insensitive identity of the filter, used to bind cursors.
    pub fn digest(&self) -> [u8; 32] {
        let archive: &[u8] = match self.archive {
            ArchiveFilter::Active => b"active",
            ArchiveFilter::Archived => b"archived",
            ArchiveFilter::All => b"all",
        };
        let mut origins = self.origins.clone();
        origins.sort();
        origins.dedup();
        let mut fields = vec![archive];
        for origin in &origins {
            fields.extend(match origin {
                OriginFilter::Interactive => [b"interactive".as_slice(), b""],
                OriginFilter::External => [b"external".as_slice(), b""],
                OriginFilter::ExternalInterface(name) => [b"interface".as_slice(), name.as_bytes()],
            });
        }
        fields.push(b"search");
        let search = self.title_search.as_deref().map(str::as_bytes);
        fields.extend(search.map_or([b"none".as_slice(), b""], |text| [b"text", text]));
        digest(b"foundry.session-list.filter.v1", &fields)
    }

    fn where_clause(&self, after_rowid: Option<i64>) -> (String, Vec<Value>) {
        let mut clauses = vec!["deleted_ms IS NULL".to_owned()];
        let mut values = Vec::new();
        match self.archive {
            ArchiveFilter::Active => clauses.push("archived=0".to_owned()),
            ArchiveFilter::Archived => clauses.push("archived=1".to_owned()),
            ArchiveFilter::All => {}
        }
        if !self.origins.is_empty() {
            let alternatives: Vec<String> = self
                .origins
                .iter()
                .map(|origin| match origin {
                    OriginFilter::Interactive => "origin_kind='interactive'".to_owned(),
                    OriginFilter::External => "origin_kind='external'".to_owned(),
                    OriginFilter::ExternalInterface(name) => {
                        values.push(Value::Text(name.clone()));
                        format!(
                            "(origin_kind='external' AND origin_interface=?{})",
                            values.len()
                        )
                    }
                })
                .collect();
            clauses.push(format!("({})", alternatives.join(" OR ")));
        }
        if let Some(rowid) = after_rowid {
            values.push(Value::Integer(rowid));
            clauses.push(format!("rowid<?{}", values.len()));
        }
        let clause = format!(" WHERE {}", clauses.join(" AND "));
        (clause, values)
    }
}

impl Store {
    /// One page of sessions, newest created first. `after` must be a cursor this
    /// filter issued; `page` is the effective size after defaults and caps.
    pub fn query_sessions(
        &self,
        filter: &SessionFilter,
        after: Option<&str>,
        page: u32,
    ) -> Result<SessionPage> {
        filter.validate(page)?;
        let binding = cursor_binding(filter);
        let after_rowid = match after {
            Some(cursor) => Some(self.cursor_rowid(cursor, &binding)?),
            None => None,
        };
        let (clause, values) = filter.where_clause(after_rowid);
        let mut statement = self.connection.prepare(&format!(
            "SELECT body,title FROM sessions{clause} ORDER BY rowid DESC"
        ))?;
        let mut rows = statement.query(params_from_iter(values))?;
        let needle = filter.title_search.as_deref().map(str::to_lowercase);
        let wanted = page as usize + 1;
        let mut sessions = Vec::with_capacity(wanted);
        while sessions.len() < wanted {
            let Some(row) = rows.next()? else { break };
            let session: Session = serde_json::from_str(&row.get::<_, String>(0)?)?;
            let title: Option<String> = row.get(1)?;
            if needle
                .as_deref()
                .is_none_or(|needle| title_matches(&session, title, needle))
            {
                sessions.push(session);
            }
        }
        let next_cursor = (sessions.len() > page as usize).then(|| {
            sessions.truncate(page as usize);
            format!(
                "{CURSOR_PREFIX}.{binding}.{}",
                sessions[sessions.len() - 1].id
            )
        });
        Ok(SessionPage {
            sessions,
            next_cursor,
        })
    }

    fn cursor_rowid(&self, cursor: &str, binding: &str) -> Result<i64> {
        let mut parts = cursor.splitn(3, '.');
        let (Some(CURSOR_PREFIX), Some(bound), Some(session_id)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid_cursor());
        };
        if bound != binding {
            return Err(invalid_cursor());
        }
        self.connection
            .query_row(
                "SELECT rowid FROM sessions WHERE id=?1",
                [session_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(invalid_cursor)
    }
}

fn cursor_binding(filter: &SessionFilter) -> String {
    filter.digest()[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The stored title column wins; otherwise the title is derived from the prompt.
fn title_matches(session: &Session, title: Option<String>, needle: &str) -> bool {
    let title = title.unwrap_or_else(|| session.prompt.chars().take(DERIVED_TITLE_CHARS).collect());
    title.to_lowercase().contains(needle)
}

fn invalid(message: &str) -> crate::StoreError {
    ContractError::Invalid(message.into()).into()
}

/// The contract has no distinct cursor class yet; the message names it.
fn invalid_cursor() -> crate::StoreError {
    invalid("invalid cursor: not issued for this session list")
}
