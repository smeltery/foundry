//! The policy document and its two operations. Get is a plain read. Replace
//! swaps the whole host list; a caller may pass the revision it last read, and
//! a stale one is `conflict` so concurrent editors cannot silently overwrite
//! each other.

use std::collections::BTreeSet;

use super::host::{Host, HostError};
use crate::authz::WorkspaceId;

/// The most extra hosts one workspace may list.
pub const MAX_HOSTS: usize = 50;

/// Increases by one on every replace that changes the host list. A workspace
/// that never saved a policy is at `Revision::INITIAL` with no hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(pub u64);

impl Revision {
    pub const INITIAL: Revision = Revision(0);
}

/// Whether the deployment currently accepts writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writes {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkPolicy {
    pub workspace: WorkspaceId,
    pub revision: Revision,
    /// Stored in the order given, lowercased, without duplicates.
    hosts: Vec<Host>,
}

/// A replace body, still unvalidated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceRequest {
    pub hosts: Vec<String>,
    /// The revision the caller last read. `None` replaces unconditionally.
    pub expected_revision: Option<Revision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyErrorClass {
    InvalidRequest,
    Conflict,
    Unavailable,
}

impl PolicyErrorClass {
    pub fn name(self) -> &'static str {
        match self {
            PolicyErrorClass::InvalidRequest => "invalid_request",
            PolicyErrorClass::Conflict => "conflict",
            PolicyErrorClass::Unavailable => "unavailable",
        }
    }
}

/// What the request got wrong. Not a wire field; indexes point into the
/// request's host list so a client can report the offending entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyIssue {
    WritesDisabled,
    TooManyHosts {
        count: usize,
    },
    InvalidHost {
        index: usize,
        error: HostError,
    },
    DuplicateHost {
        index: usize,
    },
    StaleRevision {
        current: Revision,
    },
    /// The revision counter cannot advance further.
    RevisionExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyError {
    pub class: PolicyErrorClass,
    pub issue: PolicyIssue,
}

impl PolicyError {
    fn new(class: PolicyErrorClass, issue: PolicyIssue) -> Self {
        Self { class, issue }
    }

    fn invalid(issue: PolicyIssue) -> Self {
        Self::new(PolicyErrorClass::InvalidRequest, issue)
    }
}

impl NetworkPolicy {
    /// The document of a workspace that never saved a policy.
    pub fn empty(workspace: WorkspaceId) -> Self {
        Self {
            workspace,
            revision: Revision::INITIAL,
            hosts: Vec::new(),
        }
    }

    /// The saved list of extra allowed hosts.
    pub fn hosts(&self) -> &[Host] {
        &self.hosts
    }

    /// Validates `request` against this document and returns the document to
    /// store. Rules apply in order: writes enabled, host count, each host,
    /// duplicates, then the revision. Repeating a replace that already took
    /// effect returns the stored document unchanged even with the old
    /// revision, which keeps the write naturally repeatable as specified.
    pub fn replace(&self, request: &ReplaceRequest, writes: Writes) -> Result<Self, PolicyError> {
        if writes == Writes::Disabled {
            return Err(PolicyError::new(
                PolicyErrorClass::Unavailable,
                PolicyIssue::WritesDisabled,
            ));
        }
        let count = request.hosts.len();
        if count > MAX_HOSTS {
            return Err(PolicyError::invalid(PolicyIssue::TooManyHosts { count }));
        }
        let mut seen = BTreeSet::new();
        let mut hosts = Vec::with_capacity(count);
        for (index, input) in request.hosts.iter().enumerate() {
            let host = Host::parse(input)
                .map_err(|error| PolicyError::invalid(PolicyIssue::InvalidHost { index, error }))?;
            if !seen.insert(host.clone()) {
                return Err(PolicyError::invalid(PolicyIssue::DuplicateHost { index }));
            }
            hosts.push(host);
        }
        if hosts == self.hosts {
            return Ok(self.clone());
        }
        if request
            .expected_revision
            .is_some_and(|expected| expected != self.revision)
        {
            return Err(PolicyError::new(
                PolicyErrorClass::Conflict,
                PolicyIssue::StaleRevision {
                    current: self.revision,
                },
            ));
        }
        let next = self.revision.0.checked_add(1).ok_or(PolicyError::new(
            PolicyErrorClass::Unavailable,
            PolicyIssue::RevisionExhausted,
        ))?;
        Ok(Self {
            workspace: self.workspace.clone(),
            revision: Revision(next),
            hosts,
        })
    }
}
