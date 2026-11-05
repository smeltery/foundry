//! The workspace network policy: a per-workspace document of extra allowed
//! egress hosts, its get and revision-checked replace, and a pure evaluator
//! that connectors and the tool host consult. Authorization (check order steps
//! 1 to 6) stays with `authz`; this module applies step 7, body validation and
//! the document rules. Enforcement and storage belong elsewhere. All wire names
//! are provisional pending the naming decisions.

mod document;
mod egress;
mod host;

pub use document::{
    MAX_HOSTS, NetworkPolicy, PolicyError, PolicyErrorClass, PolicyIssue, ReplaceRequest, Revision,
    Writes,
};
pub use egress::{Denied, EgressDecision, EgressPolicy, Granted};
pub use host::{Host, HostError};
