//! Pure authorization over scope, role, ownership, workspace isolation and
//! revocation, as specified in `docs/architecture/identity.md`. No storage,
//! transport or CLI: callers resolve the principal and pass a fresh snapshot of
//! committed identity state. All wire names are provisional.

mod decision;
mod ids;
mod operation;
mod organization;
mod principal;
mod scope;

pub use decision::{Access, Cause, Check, Denial, ErrorClass, OperationRequest, authorize};
pub use ids::{MachineId, PrincipalId, WorkspaceId};
pub use operation::{Cell, Operation, OperationRule};
pub use organization::{Organization, Workspace, WorkspaceState};
pub use principal::{
    Credential, CredentialKind, CredentialState, MachineBinding, Principal, PrincipalKind,
};
pub use scope::{Role, Scope};
