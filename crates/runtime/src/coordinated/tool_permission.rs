//! Trusted in-process admission, independent of adapter frames and durable claims.
use foundry_tools::host;
use std::sync::Arc;

/// Identity of the immutable helper, directory descriptor, limits and grant ceiling.
#[derive(Clone)]
pub struct ToolPolicyIdentity(Arc<()>);
impl ToolPolicyIdentity {
    pub(super) fn new() -> Self {
        Self(Arc::new(()))
    }
    pub fn same_policy(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl PartialEq for ToolPolicyIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.same_policy(other)
    }
}
impl Eq for ToolPolicyIdentity {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolAdmissionFailure {
    Invalid,
    Cancelled,
    Expired,
    Stale,
}

/// Bounded grants minted only by the trusted owner after consuming its permit.
/// This constructor is not evidence of a native or remote authorization.
pub struct ToolDispatchGrant {
    pub(super) grants: Vec<host::Grant>,
}
impl ToolDispatchGrant {
    pub fn new(grants: Vec<host::Grant>) -> Result<Self, ToolAdmissionFailure> {
        if grants.len() > 4
            || grants
                .iter()
                .enumerate()
                .any(|(i, grant)| grants[..i].contains(grant))
        {
            return Err(ToolAdmissionFailure::Invalid);
        }
        Ok(Self { grants })
    }
}

/// A unique moved authorization. Implementations must perform bounded local work,
/// validating live ownership, cancellation, expiry, binding and policy identity.
/// No database, I/O or external callbacks may run during consumption.
pub trait ToolDispatchAuthorization: Send {
    fn consume(
        self: Box<Self>,
        current_policy: &ToolPolicyIdentity,
        request: &host::Request,
    ) -> Result<ToolDispatchGrant, ToolAdmissionFailure>;
}
