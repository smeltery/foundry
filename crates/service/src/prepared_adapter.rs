//! Explicit adapter configuration and descriptor-backed tool authority composition.
mod private;
pub use private::{AdapterCredential, AdapterPrivateInputs};

use crate::{Result, Service, ServiceError, permission::*};
use foundry_contracts::{
    Session, adapter_selection::AdapterSelection, permission::PermissionProfile,
};
use foundry_runtime::{
    AdapterSpec,
    bootstrap::{BOOTSTRAP_VERSION, BootstrapIdentity, PrivateBootstrap},
    coordinated::TrustedToolPolicy,
};
use foundry_tools::host::{self, Grant};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicU8},
    time::Duration,
};

/// Trusted owner settings. Tool availability in the adapter never grants execution.
pub struct AdapterToolSettings {
    pub helper: PathBuf,
    pub grants: Vec<Grant>,
    pub missing_grant: foundry_contracts::permission::PermissionDecision,
    pub pending_mode: foundry_contracts::permission::PendingMode,
    pub request_timeout: Duration,
    pub helper_timeout: Duration,
    pub limits: host::HostLimits,
}

/// Configuration is moved once; neither private inputs nor a reusable packet escape.
/// ```compile_fail
/// use foundry_service::prepared_adapter::PreparedLocalAdapter;
/// fn log(value: &PreparedLocalAdapter) { println!("{value:?}"); }
/// ```
pub struct PreparedLocalAdapter {
    selection: AdapterSelection,
    spec: AdapterSpec,
    tools: PermissionTools,
}

impl PreparedLocalAdapter {
    pub fn new(
        mut spec: AdapterSpec,
        selection: AdapterSelection,
        private: AdapterPrivateInputs,
        tools: AdapterToolSettings,
    ) -> Result<Self> {
        let digest = selection.digest().map_err(|_| invalid())?;
        if !spec.executable.is_absolute() {
            return Err(invalid());
        }
        spec.executable = spec.executable.canonicalize().map_err(|_| invalid())?;
        if !spec.executable.is_file() {
            return Err(invalid());
        }
        let mut ceiling = Vec::new();
        for name in &selection.tools {
            match name.as_str() {
                "list" => ceiling.push(Grant::List),
                "read" => ceiling.push(Grant::Read),
                "write" => ceiling.extend([Grant::Create, Grant::Overwrite]),
                _ => return Err(invalid()),
            }
        }
        if tools.grants.iter().any(|grant| !ceiling.contains(grant)) {
            return Err(invalid());
        }
        let helper = tools.helper.canonicalize().map_err(|_| invalid())?;
        if !tools.helper.is_absolute() || !helper.is_file() {
            return Err(invalid());
        }
        let directory =
            cap_std::fs::Dir::open_ambient_dir(&selection.directory, cap_std::ambient_authority())
                .map_err(|_| invalid())?;
        let policy = TrustedToolPolicy::new(
            helper,
            directory,
            ceiling,
            tools.limits,
            tools.helper_timeout,
        )?;
        let permissions = ToolPermissions::new(
            policy.identity(),
            ToolPermissionSettings {
                profile: match selection.permission_profile.as_str() {
                    "current" => PermissionProfile::Current,
                    "legacy" => PermissionProfile::Legacy,
                    _ => return Err(invalid()),
                },
                configuration_digest: *digest.as_bytes(),
                generation: selection.generation,
                grants: tools.grants,
                missing_grant: tools.missing_grant,
                pending_mode: tools.pending_mode,
                request_timeout: tools.request_timeout,
            },
        )?;
        let tools = PermissionTools::new(policy, permissions)?;
        let bootstrap = PrivateBootstrap::new(
            BootstrapIdentity {
                schema_version: BOOTSTRAP_VERSION,
                selection_digest: digest,
            },
            private.encode(&selection)?,
        )?;
        Ok(Self {
            selection,
            spec: spec.with_private_bootstrap(bootstrap),
            tools,
        })
    }

    pub fn selection(&self) -> &AdapterSelection {
        &self.selection
    }

    /// Retain before execution; invalidate before publishing a changed selection.
    pub fn revocation(&self) -> ToolPermissionRevocation {
        self.tools.permissions.revocation()
    }

    fn check_session(&self, session: &Session) -> Result<()> {
        let Some(model) = &session.model else {
            return Err(invalid());
        };
        if session.directory != self.selection.directory
            || model.provider != self.selection.provider
            || model.model != self.selection.model
            || model.effort.as_deref() != Some(&self.selection.effort)
        {
            return Err(invalid());
        }
        Ok(())
    }
}

impl Service {
    /// Verify stored selection before attempt admission, then require consuming
    /// tool permissions. SDK configuration is acknowledged before Start, not before
    /// the existing service attempt transaction. Failed preparation is not replayed.
    pub fn run_prepared(
        &mut self,
        id: &str,
        expected_version: u64,
        prepared: PreparedLocalAdapter,
        control: Arc<AtomicU8>,
    ) -> Result<Session> {
        prepared.check_session(&self.store.get(id)?)?;
        self.run_with_permissions(
            id,
            expected_version,
            &prepared.spec,
            control,
            prepared.tools,
        )
    }
}

fn invalid() -> ServiceError {
    ServiceError::Invalid("invalid prepared adapter configuration")
}

#[cfg(test)]
mod tests;
