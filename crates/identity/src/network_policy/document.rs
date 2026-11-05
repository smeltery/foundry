//! The policy document and its two operations. Get is a plain read. Replace
//! swaps the whole host list and requires the revision the caller last read;
//! a revision that differs from this document's is `conflict`.
//!
//! This layer checks the revision against the document it is given, which
//! alone cannot stop a lost update: two replaces computed from the same
//! document both succeed here. The store must commit a `Replacement` only if
//! its stored revision still equals `based_on`, atomically, and answer
//! `conflict` otherwise.

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
    /// The revision the caller last read. Required; `None` models a body that
    /// omits it, which is `invalid_request`.
    pub expected_revision: Option<Revision>,
}

/// A validated replace, for the store to commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    /// The revision of the document this was computed from: the store's
    /// compare-and-swap condition.
    pub based_on: Revision,
    /// The document to store and return.
    pub policy: NetworkPolicy,
}

impl Replacement {
    /// False when the request repeats a replace that already took effect:
    /// there is nothing to write, and the stored document is the response.
    pub fn changed(&self) -> bool {
        self.policy.revision != self.based_on
    }
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
    MissingRevision,
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

    /// Validates `request` against this document. Rules apply in order:
    /// writes enabled, revision present, host count, each host, duplicates,
    /// then the revision value. Hosts equal to the stored list give an
    /// unchanged `Replacement` even with an older revision. That keeps the
    /// write naturally repeatable, as the spec requires, and loses nothing.
    pub fn replace(
        &self,
        request: &ReplaceRequest,
        writes: Writes,
    ) -> Result<Replacement, PolicyError> {
        if writes == Writes::Disabled {
            return Err(PolicyError::new(
                PolicyErrorClass::Unavailable,
                PolicyIssue::WritesDisabled,
            ));
        }
        let expected = request
            .expected_revision
            .ok_or(PolicyError::invalid(PolicyIssue::MissingRevision))?;
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
        let mut policy = self.clone();
        if hosts != self.hosts {
            if expected != self.revision {
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
            policy.revision = Revision(next);
            policy.hosts = hosts;
        }
        Ok(Replacement {
            based_on: self.revision,
            policy,
        })
    }
}
