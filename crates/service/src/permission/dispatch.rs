use super::*;
use crate::permission_gate::{GateFailure, PermissionGate};
use foundry_contracts::permission::{PermissionError, PermissionPermit};
use foundry_runtime::coordinated::{ToolAdmissionFailure, ToolDispatchGrant};

pub(super) struct Authorization {
    pub request: PermissionRequest,
    pub permit: PermissionPermit,
    pub binding: PermissionBinding,
    pub started: Instant,
    pub generation: u64,
    pub selection_generation: u64,
    pub gate: PermissionGate,
    pub current: Arc<Mutex<Option<u64>>>,
    pub identity: ToolPolicyIdentity,
    pub grant: host::Grant,
}
impl ToolDispatchAuthorization for Authorization {
    fn consume(
        self: Box<Self>,
        current_policy: &ToolPolicyIdentity,
        host_request: &host::Request,
    ) -> std::result::Result<ToolDispatchGrant, ToolAdmissionFailure> {
        let Self {
            mut request,
            permit,
            binding,
            started,
            generation,
            selection_generation,
            gate,
            current,
            identity,
            grant,
        } = *self;
        let selected = current.lock().map_err(|_| ToolAdmissionFailure::Stale)?;
        if *selected != Some(selection_generation) || !identity.same_policy(current_policy) {
            return Err(ToolAdmissionFailure::Stale);
        }
        if host_request.version != host::VERSION {
            return Err(ToolAdmissionFailure::Invalid);
        }
        let actual = PermissionBinding {
            session_id: binding.session_id.clone(),
            attempt_id: host_request.attempt_id.clone(),
            call_id: host_request.call_id.clone(),
            tool: host_request.tool.clone(),
            input_digest: tool_digest::input(&host_request.tool, &host_request.input)
                .map_err(|_| ToolAdmissionFailure::Invalid)?,
            profile: binding.profile,
        };
        gate.consume(generation, || {
            let now = u64::try_from(started.elapsed().as_millis())
                .map_err(|_| ToolAdmissionFailure::Expired)?;
            request
                .consume(permit, &actual, now)
                .map_err(|error| match error {
                    PermissionError::Expired => ToolAdmissionFailure::Expired,
                    PermissionError::Cancelled => ToolAdmissionFailure::Cancelled,
                    PermissionError::Stale => ToolAdmissionFailure::Stale,
                    _ => ToolAdmissionFailure::Invalid,
                })?;
            ToolDispatchGrant::new(vec![grant])
        })
        .map_err(|error| match error {
            GateFailure::Expired => ToolAdmissionFailure::Expired,
            GateFailure::Revoked => ToolAdmissionFailure::Cancelled,
            GateFailure::Stale => ToolAdmissionFailure::Stale,
            _ => ToolAdmissionFailure::Invalid,
        })?
    }
}
