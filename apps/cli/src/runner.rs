//! Explicitly configured local adapter execution over the application service.
mod configuration;
#[cfg(unix)]
mod native;
mod native_registration;
mod prepared;
mod task;
mod tool_policy;
use crate::{Backend, CommandError, Request, Response};
use foundry_contracts::{ContractError, CreateSession, Lifecycle, Outcome, Session};
use foundry_runtime::AdapterSpec;
use foundry_service::{Service, ServiceError};
use foundry_store::StoreError;
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicU8};
use task::{fields, object, string};

/// Delegates non-execution operations to the existing backend.
/// Merely constructing this wrapper does not open a database or run a process.
pub struct ExecutionBackend<B> {
    database: PathBuf,
    fallback: B,
    adapter: Option<PathBuf>,
    adapter_configuration: Option<PathBuf>,
    native_profile: Option<PathBuf>,
    toolhost_override: Option<PathBuf>,
    control: Arc<AtomicU8>,
}

impl<B: Backend> ExecutionBackend<B> {
    pub fn new(
        database: impl Into<PathBuf>,
        fallback: B,
        adapter: Option<PathBuf>,
    ) -> Result<Self, CommandError> {
        if adapter.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err(invalid("adapter executable must be an absolute path"));
        }
        Ok(Self {
            database: database.into(),
            fallback,
            adapter,
            adapter_configuration: None,
            native_profile: None,
            toolhost_override: None,
            control: Arc::new(AtomicU8::new(0)),
        })
    }

    /// The environment value is one executable path, never a command line.
    pub fn from_env(database: impl Into<PathBuf>, fallback: B) -> Result<Self, CommandError> {
        Self::new(
            database,
            fallback,
            std::env::var_os("FOUNDRY_ADAPTER").map(PathBuf::from),
        )
        .map(|mut backend| {
            backend.adapter_configuration =
                std::env::var_os("FOUNDRY_ADAPTER_CONFIG").map(PathBuf::from);
            backend.toolhost_override = std::env::var_os("FOUNDRY_TOOLHOST").map(PathBuf::from);
            backend.native_profile = std::env::var_os("FOUNDRY_NATIVE_PROFILE").map(PathBuf::from);
            backend
        })
    }

    /// Store a private configuration locator; load it only for a local execution.
    pub fn with_adapter_configuration(mut self, path: PathBuf) -> Self {
        self.adapter_configuration = Some(path);
        self
    }

    /// Share startup's signal control without installing handlers in library users.
    pub fn with_control(mut self, control: Arc<AtomicU8>) -> Self {
        self.control = control;
        self
    }

    /// Explicit trusted development helper override; release provenance is not verified.
    /// Prepared execution always validates it; legacy execution uses it only with grants.
    pub fn with_toolhost(mut self, executable: PathBuf) -> Self {
        self.toolhost_override = Some(executable);
        self
    }

    fn run(
        &self,
        request: CreateSession,
        grants: Vec<foundry_tools::host::Grant>,
    ) -> Result<Response, CommandError> {
        request.validate().map_err(contract_error)?;
        let executable = self.adapter.as_ref().ok_or_else(|| {
            CommandError::new(
                "adapter_not_configured",
                "Set FOUNDRY_ADAPTER to an absolute executable path to run a local task",
            )
        })?;
        let executable = executable
            .canonicalize()
            .map_err(|_| invalid("adapter executable is unavailable"))?;
        if !executable.is_file() {
            return Err(invalid("adapter executable must be a file"));
        }
        let policy = tool_policy::build(
            &request.directory,
            grants,
            self.toolhost_override.as_deref(),
        )?;
        let mut service = Service::open(&self.database).map_err(service_error)?;
        let queued = service.create(request).map_err(service_error)?;
        let spec = AdapterSpec::new(executable);
        let result = match policy {
            Some(policy) => service.run_with_tools(
                &queued.id,
                queued.version,
                &spec,
                Arc::clone(&self.control),
                policy,
            ),
            None => service.run_with_control(
                &queued.id,
                queued.version,
                &spec,
                Arc::clone(&self.control),
            ),
        };
        finish(result, &queued.id)
    }
}

impl<B: Backend> Backend for ExecutionBackend<B> {
    fn execute(&mut self, mut request: Request) -> Result<Response, CommandError> {
        if request.version != 1 {
            return Err(unsupported("request version"));
        }
        let command = object(&request.command, "command")?;
        fields(command, &["operation", "arguments"])?;
        let operation = string(command, "operation")?;
        let start = operation == "session" && request.command["arguments"]["operation"] == "start";
        if operation != "prompt" && operation != "run" && !start {
            return self.fallback.execute(request);
        }
        if operation == "prompt" && !request.command["arguments"]["options"]["resume"].is_null() {
            let id = request.command["arguments"]["options"]["resume"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| invalid("invalid resume session"))?
                .to_owned();
            if request.command["arguments"]["prompt"].is_null() {
                existing_database(&self.database)?;
                request.command = json!({"operation":"session","arguments":{"operation":"get","arguments":{"id":id}}});
                return self.fallback.execute(request);
            }
            #[cfg(unix)]
            return native::resume(self, request, &id);
            #[cfg(not(unix))]
            return Err(unsupported("native profile platform"));
        }
        let task_options = task::options(&request, operation, start);
        if self.native_profile.is_some() && self.adapter_configuration.is_none() {
            return Err(invalid(
                "native profile requires private adapter configuration",
            ));
        }
        if self.adapter_configuration.is_none()
            && task_options
                .get("effort")
                .is_some_and(|value| !value.is_null())
        {
            return Err(unsupported("option effort"));
        }
        let grants = &task_options["tool_grants"];
        let grants = tool_policy::grants(grants)?;
        // Preserve the store-only queue contract when execution was not configured.
        if start
            && self.adapter.is_none()
            && self.adapter_configuration.is_none()
            && grants.is_empty()
        {
            return self.fallback.execute(request);
        }
        let task = task::resolve(&request, self.adapter_configuration.is_some())?;
        if let Some(path) = &self.adapter_configuration {
            let executable = self.adapter.as_deref().ok_or_else(|| {
                invalid("private adapter configuration requires an adapter executable")
            })?;
            if let Some(registration) = &self.native_profile {
                #[cfg(unix)]
                return native::run(
                    &self.database,
                    registration,
                    prepared::resolve(task, executable, path, self.toolhost_override.as_deref())?,
                    &self.control,
                    None,
                );
                #[cfg(not(unix))]
                return Err(unsupported("native profile platform"));
            }
            let resolved =
                prepared::resolve(task, executable, path, self.toolhost_override.as_deref())?;
            let snapshots = foundry_service::cancellation_snapshot::SnapshotSettings::for_database(
                resolved.helper().to_path_buf(),
                &self.database,
            )
            .map_err(service_error)?;
            let request = resolved.request.clone();
            let prepared = resolved.prepare(None)?;
            let mut service = Service::open(&self.database)
                .map_err(service_error)?
                .with_workspace_snapshots(snapshots);
            let queued = service.create(request).map_err(service_error)?;
            let result = service.run_prepared(
                &queued.id,
                queued.version,
                prepared,
                Arc::clone(&self.control),
            );
            return finish(result, &queued.id);
        }
        self.run(
            CreateSession {
                prompt: task.prompt,
                directory: task.directory,
                model: None,
                idempotency_key: None,
            },
            task.grants,
        )
    }
}

fn finish(result: Result<Session, ServiceError>, id: &str) -> Result<Response, CommandError> {
    let session = result.map_err(|error| {
        let mut error = service_error(error);
        error.message = format!("{} (session {})", error.message, id);
        error
    })?;
    terminal_response(session)
}

fn terminal_response(session: Session) -> Result<Response, CommandError> {
    let (exit_code, text) = match (session.lifecycle, session.outcome) {
        (Lifecycle::Exit, Some(outcome)) => {
            let code = match outcome {
                Outcome::Success => 0,
                Outcome::Failure => 1,
                Outcome::ActionRequired => 3,
            };
            (code, session.result.clone().unwrap_or_default())
        }
        (Lifecycle::Suspended, None) => (130, "Session suspended".into()),
        _ => {
            return Err(CommandError::new(
                "execution",
                "adapter did not produce a valid terminal session",
            ));
        }
    };
    Ok(Response {
        text,
        json: json!({"session":session,"execution":"finished","target":"local"}),
        exit_code,
    })
}

fn invalid(message: &str) -> CommandError {
    CommandError::new("invalid", message)
}
fn existing_database(path: &std::path::Path) -> Result<(), CommandError> {
    if !path.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Err(CommandError::new(
            "storage",
            "database must be an existing file",
        ));
    }
    Ok(())
}
fn unsupported(message: &str) -> CommandError {
    CommandError::new(
        "unsupported",
        format!("Local adapter execution does not support {message}"),
    )
}
fn contract_error(error: ContractError) -> CommandError {
    let code = match &error {
        ContractError::Invalid(_) => "invalid",
        ContractError::Conflict(_) => "conflict",
        ContractError::NotFound(_) => "not_found",
        ContractError::Unsupported(_) => "unsupported",
    };
    CommandError::new(code, error.to_string())
}
fn service_error(error: ServiceError) -> CommandError {
    match error {
        ServiceError::Store(StoreError::Contract(error)) => contract_error(error),
        ServiceError::Store(error) => CommandError::new("storage", error.to_string()),
        ServiceError::Invalid(message) => invalid(message),
        other => CommandError::new("execution", other.to_string()),
    }
}
