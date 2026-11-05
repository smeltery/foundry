//! Additive session contract v2: optional create options, session metadata and
//! the cancel receipt. This is an interface gate only; no option changes
//! behavior until the change that implements it lands (see
//! `docs/architecture/session-api.md`). All wire names and bounds below are
//! provisional pending the naming decision.
//!
//! Every new field is optional and omitted from the encoding when absent, so a
//! request or session written before v2 serializes to the same bytes. The store
//! compares those bytes for idempotent create replays.
//!
//! Pending user decisions, deliberately not encoded here:
//! - the terminal lifecycle for cancelling an already suspended session, so no
//!   `Lifecycle` variant is added;
//! - delete semantics (hard delete or tombstone);
//! - any HTTP transport and its routes.
use crate::{CancelMode, ContractError, CreateSession, Lifecycle};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::Component, path::Path};

/// Identifier of the approved completion policy (`completion-policy.md`).
pub const COMPLETION_POLICY_VERSION: &str = "foundry.completion.v1";
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_TAGS: usize = 64;
pub const MAX_LABEL_BYTES: usize = 64;
pub const MAX_REFERENCE_BYTES: usize = 256;
pub const MAX_COMPLETION_CHECKS: usize = 32;
pub const MAX_CHECK_ARGUMENTS: usize = 64;
pub const MAX_CHECK_TIMEOUT_MS: u64 = 600_000;

/// Where the session works: a repository checkout or a repository-neutral start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceSelector {
    /// Empty braces, not a unit variant: serde ignores extra keys on tagged unit variants.
    Scratch {},
    Repository {
        repository: String,
        checkout: Checkout,
    },
}

/// Exactly one checkout selector; the single-key encoding makes a second one
/// unrepresentable rather than silently preferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Checkout {
    Branch(String),
    Reference(String),
    ChangeRequest(u64),
}

/// Execution target. Only machine targets are specified; an unavailable target
/// must fail without fallback (NIC-37 owns enrollment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionTarget {
    pub kind: TargetKind,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Machine,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionOrigin {
    Interactive {},
    External {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interface: Option<String>,
    },
}

/// Criteria declared before an attempt runs. A named profile supplies defaults;
/// an inline check whose `id` matches a profile check overrides it, and any
/// other inline check is added. The admitted set is fixed per attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionCriteria {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CompletionCheck>,
}

/// v1 check kinds. The runtime runs them under the attempt fence; a check
/// grants no permissions and the model cannot add, select or widen one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompletionCheck {
    /// Passes only when `argv` exits with status 0 within `timeout_ms`. No shell.
    Command {
        id: String,
        argv: Vec<String>,
        timeout_ms: u64,
    },
    /// Passes when the workspace-relative `path` exists and, when `sha256` is
    /// set, its content has that lowercase hexadecimal digest.
    File {
        id: String,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
    },
}

impl CompletionCriteria {
    pub fn validate(&self) -> Result<(), ContractError> {
        validate_criteria(self)
    }
}

impl CompletionCheck {
    pub fn id(&self) -> &str {
        match self {
            Self::Command { id, .. } | Self::File { id, .. } => id,
        }
    }
}

/// Session metadata carried flat in the session encoding.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadata {
    #[serde(default, skip_serializing_if = "is_false")]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SessionOrigin>,
    /// Milliseconds since the Unix epoch; absent for sessions written before v2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_ms: Option<u64>,
    /// Owning session of a child session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// Source of a fork. The source session is never changed by forking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from_session_id: Option<String>,
}

/// Result of a cancel call. Cancellation of a running attempt is asynchronous:
/// callers poll until the session is no longer running. Cancelling a suspended
/// session reports the terminal `cancelled` status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelReceipt {
    pub ok: bool,
    /// Whether this call newly recorded or upgraded the cancellation.
    pub requested: bool,
    /// Effective mode after the call; hard never downgrades to graceful.
    pub mode: CancelMode,
    /// Lifecycle after the call.
    pub status: Lifecycle,
}

impl CreateSession {
    /// Names of the v2 options this request sets, in declaration order.
    pub fn v2_options(&self) -> Vec<&'static str> {
        [
            ("workspace", self.workspace.is_some()),
            ("tags", !self.tags.is_empty()),
            ("title", self.title.is_some()),
            ("target", self.target.is_some()),
            ("tool_profile", self.tool_profile.is_some()),
            ("origin", self.origin.is_some()),
            ("completion", self.completion.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, set)| set.then_some(name))
        .collect()
    }

    pub(crate) fn validate_v2(&self) -> Result<(), ContractError> {
        if let Some(WorkspaceSelector::Repository {
            repository,
            checkout,
        }) = &self.workspace
        {
            label("repository", repository, MAX_REFERENCE_BYTES)?;
            match checkout {
                Checkout::Branch(name) => label("branch", name, MAX_REFERENCE_BYTES)?,
                Checkout::Reference(name) => label("reference", name, MAX_REFERENCE_BYTES)?,
                Checkout::ChangeRequest(0) => {
                    return Err(invalid("change request must be positive"));
                }
                Checkout::ChangeRequest(_) => {}
            }
        }
        if self.tags.len() > MAX_TAGS {
            return Err(invalid("too many tags"));
        }
        let mut tags = HashSet::new();
        for tag in &self.tags {
            label("tag", tag, MAX_LABEL_BYTES)?;
            if !tags.insert(tag) {
                return Err(invalid("tags must be distinct"));
            }
        }
        if let Some(title) = &self.title
            && (title.trim().is_empty() || title.chars().count() > MAX_TITLE_CHARS)
        {
            return Err(invalid("title must contain 1 to 200 characters"));
        }
        if let Some(target) = &self.target {
            label("target name", &target.name, MAX_LABEL_BYTES)?;
        }
        if let Some(profile) = &self.tool_profile {
            label("tool profile", profile, MAX_REFERENCE_BYTES)?;
            reference("tool profile", profile)?;
        }
        if let Some(SessionOrigin::External {
            interface: Some(interface),
        }) = &self.origin
        {
            label("origin interface", interface, MAX_LABEL_BYTES)?;
        }
        self.completion.as_ref().map_or(Ok(()), validate_criteria)
    }
}

fn validate_criteria(criteria: &CompletionCriteria) -> Result<(), ContractError> {
    if let Some(profile) = &criteria.profile {
        label("criteria profile", profile, MAX_REFERENCE_BYTES)?;
    } else if criteria.checks.is_empty() {
        return Err(invalid("completion criteria need a profile or a check"));
    }
    if criteria.checks.len() > MAX_COMPLETION_CHECKS {
        return Err(invalid("too many completion checks"));
    }
    let mut ids = HashSet::new();
    for check in &criteria.checks {
        label("check id", check.id(), MAX_LABEL_BYTES)?;
        if !ids.insert(check.id()) {
            return Err(invalid("completion check ids must be distinct"));
        }
        match check {
            CompletionCheck::Command {
                argv, timeout_ms, ..
            } => {
                if argv.is_empty() || argv.len() > MAX_CHECK_ARGUMENTS || argv[0].trim().is_empty()
                {
                    return Err(invalid("command check needs 1 to 64 arguments"));
                }
                if argv
                    .iter()
                    .any(|argument| argument.len() > MAX_REFERENCE_BYTES)
                {
                    return Err(invalid("command check argument is too long"));
                }
                if !(1..=MAX_CHECK_TIMEOUT_MS).contains(timeout_ms) {
                    return Err(invalid("command check timeout must be 1 to 600000 ms"));
                }
            }
            CompletionCheck::File { path, sha256, .. } => {
                label("file check path", path, MAX_REFERENCE_BYTES)?;
                if !Path::new(path)
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
                {
                    return Err(invalid("file check path must stay inside the workspace"));
                }
                if sha256.as_ref().is_some_and(|digest| {
                    digest.len() != 64
                        || !digest
                            .bytes()
                            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                }) {
                    return Err(invalid("file check digest must be lowercase SHA-256 hex"));
                }
            }
        }
    }
    Ok(())
}

fn label(field: &str, value: &str, max_bytes: usize) -> Result<(), ContractError> {
    if value.trim().is_empty() || value.len() > max_bytes {
        return Err(ContractError::Invalid(format!(
            "{field} must contain 1 to {max_bytes} UTF-8 bytes"
        )));
    }
    Ok(())
}

/// A reference is `/`-separated segments of lowercase ASCII letters, digits,
/// `-`, `_` and `.`, with no empty, `.` or `..` segment.
fn reference(field: &str, value: &str) -> Result<(), ContractError> {
    let segment_ok = |segment: &str| {
        !matches!(segment, "" | "." | "..")
            && segment
                .bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.'))
    };
    if !value.split('/').all(segment_ok) {
        return Err(ContractError::Invalid(format!(
            "{field} must be lowercase segments of letters, digits, '-', '_' or '.'"
        )));
    }
    Ok(())
}

fn invalid(message: &str) -> ContractError {
    ContractError::Invalid(message.into())
}

fn is_false(value: &bool) -> bool {
    !value
}
