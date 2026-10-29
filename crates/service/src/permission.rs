//! Trusted local tool decisions, consumed only at the runtime helper boundary.
mod dispatch;

use crate::{Result, ServiceError, control::RunControl, tool_digest};
use foundry_contracts::permission::{
    PendingMode, PermissionBinding, PermissionDecision, PermissionDisposition, PermissionRequest,
};
use foundry_runtime::coordinated::{ToolDispatchAuthorization, ToolIntent, ToolPolicyIdentity};
use foundry_store::AttemptFence;
use foundry_tools::host;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Explicit owner inputs. No environment or adapter frame contributes authority.
pub struct ToolPermissionSettings {
    pub profile: foundry_contracts::permission::PermissionProfile,
    pub configuration_digest: [u8; 32],
    pub generation: u64,
    pub grants: Vec<host::Grant>,
    pub missing_grant: PermissionDecision,
    pub pending_mode: PendingMode,
    pub request_timeout: Duration,
}

pub struct ToolPermissions {
    settings: ToolPermissionSettings,
    identity: ToolPolicyIdentity,
    current: Arc<Mutex<Option<u64>>>,
}

/// Revokes the complete effective configuration, including unchanged profile labels.
#[derive(Clone)]
pub struct ToolPermissionRevocation(Arc<Mutex<Option<u64>>>);
impl ToolPermissionRevocation {
    pub fn invalidate(&self) {
        if let Ok(mut current) = self.0.lock() {
            *current = None;
        }
    }
}

/// Required-mode runtime policy and its matching service authority selection.
pub struct PermissionTools {
    pub(crate) policy: foundry_runtime::coordinated::TrustedToolPolicy,
    pub(crate) permissions: Arc<ToolPermissions>,
}
impl PermissionTools {
    pub fn new(
        policy: foundry_runtime::coordinated::TrustedToolPolicy,
        permissions: ToolPermissions,
    ) -> Result<Self> {
        permissions.check_policy(&policy.identity())?;
        Ok(Self {
            policy: policy.require_authorization(),
            permissions: Arc::new(permissions),
        })
    }
    pub fn configuration_digest(&self) -> [u8; 32] {
        self.permissions.configuration_digest()
    }
}

pub(crate) enum PreparedPermission {
    Authorized(Box<dyn ToolDispatchAuthorization>),
    Blocked(PermissionDisposition),
}

impl ToolPermissions {
    pub fn new(identity: ToolPolicyIdentity, settings: ToolPermissionSettings) -> Result<Self> {
        if settings.grants.len() > 4
            || settings
                .grants
                .iter()
                .enumerate()
                .any(|(i, grant)| settings.grants[..i].contains(grant))
            || settings.missing_grant == PermissionDecision::Allow
            || settings.pending_mode == PendingMode::Interactive
            || settings.request_timeout.is_zero()
            || settings.request_timeout.as_millis() == 0
            || settings.request_timeout.as_millis() > u64::MAX as u128
        {
            return Err(ServiceError::Invalid("invalid tool permission selection"));
        }
        let generation = settings.generation;
        Ok(Self {
            settings,
            identity,
            current: Arc::new(Mutex::new(Some(generation))),
        })
    }

    pub fn revocation(&self) -> ToolPermissionRevocation {
        ToolPermissionRevocation(Arc::clone(&self.current))
    }

    pub fn configuration_digest(&self) -> [u8; 32] {
        self.settings.configuration_digest
    }

    pub(crate) fn check_policy(&self, policy: &ToolPolicyIdentity) -> Result<()> {
        let current = self.current.lock().map_err(|_| invalid())?;
        if *current != Some(self.settings.generation) || !self.identity.same_policy(policy) {
            return Err(invalid());
        }
        Ok(())
    }

    pub(crate) fn check_current(&self) -> Result<()> {
        self.check_policy(&self.identity)
    }

    pub(crate) fn check_configuration(&self, digest: &[u8; 32]) -> Result<()> {
        self.check_current()?;
        if self.configuration_digest() != *digest {
            return Err(ServiceError::Invalid(
                "capture permission configuration mismatch",
            ));
        }
        Ok(())
    }

    /// Admit only bounded in-memory terminal work while the selection stays current.
    /// The callback must never perform I/O or retain this lock through storage.
    pub(crate) fn with_current_configuration<T>(
        &self,
        digest: &[u8; 32],
        admit: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let current = self.current.lock().map_err(|_| invalid())?;
        if *current != Some(self.settings.generation) || self.configuration_digest() != *digest {
            return Err(invalid());
        }
        admit()
    }

    pub(crate) fn prepare(
        &self,
        fence: &AttemptFence,
        intent: &ToolIntent,
        signals: &RunControl,
    ) -> Result<PreparedPermission> {
        let current = self.current.lock().map_err(|_| invalid())?;
        if *current != Some(self.settings.generation) {
            signals.permissions.invalidate();
            return Err(invalid());
        }
        let grant = required(&intent.tool, &intent.input)?;
        let binding = PermissionBinding {
            session_id: fence.session_id.clone(),
            attempt_id: fence.attempt_id.clone(),
            call_id: intent.call_id.clone(),
            tool: intent.tool.clone(),
            input_digest: tool_digest::input(&intent.tool, &intent.input)?,
            profile: self.settings.profile,
        };
        let generation = signals.permissions.generation().map_err(|_| invalid())?;
        let started = Instant::now();
        let mut request = PermissionRequest::new(
            binding.clone(),
            self.settings.request_timeout.as_millis() as u64,
            0,
        )
        .map_err(|_| invalid())?;
        let decision = if self.settings.grants.contains(&grant) {
            PermissionDecision::Allow
        } else {
            self.settings.missing_grant
        };
        signals
            .permissions
            .consume(generation, || request.decide(&binding, decision, 0))
            .map_err(|_| invalid())?
            .map_err(|_| invalid())?;
        if decision != PermissionDecision::Allow {
            let disposition = request
                .disposition(self.settings.pending_mode, 0)
                .map_err(|_| invalid())?;
            return Ok(PreparedPermission::Blocked(disposition));
        }
        let permit = request.take_permit(&binding, 0).map_err(|_| invalid())?;
        Ok(PreparedPermission::Authorized(Box::new(
            dispatch::Authorization {
                request,
                permit,
                binding,
                started,
                generation,
                selection_generation: self.settings.generation,
                gate: signals.permissions.clone(),
                current: Arc::clone(&self.current),
                identity: self.identity.clone(),
                grant,
            },
        )))
    }
}

fn required(tool: &str, input: &host::Input) -> Result<host::Grant> {
    match (tool, input) {
        ("read", host::Input::Path(_)) => Ok(host::Grant::Read),
        ("list", host::Input::Path(_)) => Ok(host::Grant::List),
        ("write", host::Input::Write(args)) => Ok(match args.mode {
            host::WriteMode::Create => host::Grant::Create,
            host::WriteMode::Overwrite => host::Grant::Overwrite,
        }),
        _ => Err(invalid()),
    }
}
fn invalid() -> ServiceError {
    ServiceError::Coordination("tool permission selection is unavailable")
}

#[cfg(test)]
mod tests;
