//! The pure egress evaluator connectors and the tool host consult before
//! opening a connection. An execution captures an `EgressPolicy` when it
//! starts and keeps it: a later replace changes only executions started after
//! it, and never revokes access a running execution already holds.

use std::collections::BTreeSet;

use super::document::NetworkPolicy;
use super::host::{Host, HostError};

/// The allowed hosts one execution was started with: the platform baseline
/// plus the workspace's extra hosts. Workspace hosts only extend the baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressPolicy {
    baseline: BTreeSet<Host>,
    workspace: BTreeSet<Host>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granted {
    Baseline,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// The destination is not a host the grammar accepts.
    Unparsed(HostError),
    /// A valid host that neither the baseline nor the workspace lists.
    NotListed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressDecision {
    Allow(Granted),
    Deny(Denied),
}

impl EgressPolicy {
    /// Captures the policy for an execution starting now.
    pub fn at_execution_start(
        baseline: impl IntoIterator<Item = Host>,
        policy: &NetworkPolicy,
    ) -> Self {
        Self {
            baseline: baseline.into_iter().collect(),
            workspace: policy.hosts().iter().cloned().collect(),
        }
    }

    /// Decides one destination host, as a bare name without scheme or port.
    /// Anything that does not parse, or is not listed, is denied.
    pub fn evaluate(&self, destination: &str) -> EgressDecision {
        let host = match Host::parse(destination) {
            Ok(host) => host,
            Err(error) => return EgressDecision::Deny(Denied::Unparsed(error)),
        };
        if self.baseline.contains(&host) {
            EgressDecision::Allow(Granted::Baseline)
        } else if self.workspace.contains(&host) {
            EgressDecision::Allow(Granted::Workspace)
        } else {
            EgressDecision::Deny(Denied::NotListed)
        }
    }
}
