//! Resolve one immutable fresh selection before opening session storage.
use super::{configuration, contract_error, invalid, service_error, task::LocalTask, tool_policy};
use crate::CommandError;
use foundry_contracts::adapter_selection::AdapterSelection;
use foundry_contracts::{
    CreateSession, ModelConfig,
    permission::{PendingMode, PermissionDecision},
};
use foundry_runtime::AdapterSpec;
use foundry_service::prepared_adapter::{
    AdapterPrivateInputs, AdapterToolSettings, PreparedLocalAdapter,
};
use std::path::Path;

pub(super) struct Resolved {
    pub request: CreateSession,
    pub selection: AdapterSelection,
    pub private: AdapterPrivateInputs,
    tools: AdapterToolSettings,
    executable: std::path::PathBuf,
}
impl Resolved {
    pub fn helper(&self) -> &Path {
        &self.tools.helper
    }
    pub fn prepare(self, plan: Option<&Path>) -> Result<PreparedLocalAdapter, CommandError> {
        let mut spec = AdapterSpec::new(&self.executable);
        if let Some(path) = plan {
            spec.args = vec![path.as_os_str().into()];
        }
        PreparedLocalAdapter::new(spec, self.selection, self.private, self.tools)
            .map_err(service_error)
    }
}
pub(super) fn resolve(
    task: LocalTask,
    executable: &Path,
    configuration_path: &Path,
    helper_override: Option<&Path>,
) -> Result<Resolved, CommandError> {
    let configuration::LocalAdapterConfiguration {
        mut selection,
        private,
    } = configuration::load(configuration_path)?;
    let workspace = Path::new(&selection.directory)
        .canonicalize()
        .map_err(|_| invalid("configured workspace is unavailable"))?;
    if !workspace.is_dir()
        || workspace.to_str() != Some(selection.directory.as_str())
        || task.directory != selection.directory
    {
        return Err(invalid(
            "configured workspace does not match task directory",
        ));
    }
    if !private.agent_directory.is_dir() {
        return Err(invalid("configured agent directory is unavailable"));
    }
    // CLI values override only these fields for this fresh, one-use selection.
    // Provider and opaque owner-issued labels are preserved, never inferred.
    if let Some(model) = task.model {
        selection.model = model;
    }
    if let Some(effort) = task.effort {
        selection.effort = effort;
    }
    let request = CreateSession {
        prompt: task.prompt,
        directory: selection.directory.clone(),
        model: Some(ModelConfig {
            provider: selection.provider.clone(),
            model: selection.model.clone(),
            effort: Some(selection.effort.clone()),
        }),
        idempotency_key: None,
    };
    request.validate().map_err(contract_error)?;
    let tools = AdapterToolSettings {
        helper: tool_policy::helper_path(helper_override)?,
        grants: task.grants,
        missing_grant: PermissionDecision::Pending,
        pending_mode: PendingMode::ActionRequired,
        request_timeout: tool_policy::TOOL_TIMEOUT,
        helper_timeout: tool_policy::TOOL_TIMEOUT,
        limits: tool_policy::limits(),
    };
    Ok(Resolved {
        request,
        selection,
        private,
        tools,
        executable: executable.into(),
    })
}
