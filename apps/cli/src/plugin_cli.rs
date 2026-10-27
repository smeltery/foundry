//! Shell-only scope checks. Project discovery and package validation require a backend.
use crate::{
    CommandError,
    plugin_args::{PluginCommand, PluginListScope, PluginMutationScope},
};

pub(crate) fn validate(command: &PluginCommand) -> Result<(), CommandError> {
    let invalid_project = match command {
        PluginCommand::List { project, scope, .. } => {
            project.is_some() && *scope == PluginListScope::User
        }
        PluginCommand::Add { project, scope, .. }
        | PluginCommand::Remove { project, scope, .. } => {
            project.is_some() && *scope == PluginMutationScope::User
        }
    };
    if invalid_project {
        return Err(CommandError::new(
            "usage",
            "--project requires project scope, or all scope for listing",
        ));
    }
    Ok(())
}
