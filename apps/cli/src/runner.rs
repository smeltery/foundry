//! Explicitly configured local adapter execution over the application service.
mod tool_policy;
use crate::{Backend, CommandError, Request, Response};
use foundry_contracts::{ContractError, CreateSession, Lifecycle, Outcome, Session};
use foundry_runtime::AdapterSpec;
use foundry_service::{Service, ServiceError};
use foundry_store::StoreError;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicU8};

/// Delegates non-execution operations to the existing backend.
/// Merely constructing this wrapper does not open a database or run a process.
pub struct ExecutionBackend<B> {
    database: PathBuf,
    fallback: B,
    adapter: Option<PathBuf>,
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
            backend.toolhost_override = std::env::var_os("FOUNDRY_TOOLHOST").map(PathBuf::from);
            backend
        })
    }

    /// Share startup's signal control without installing handlers in library users.
    pub fn with_control(mut self, control: Arc<AtomicU8>) -> Self {
        self.control = control;
        self
    }

    /// Explicit trusted development helper override; release provenance is not verified.
    /// Only used with nonempty grants; validated before opening the session database.
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
        let session = result.map_err(|error| {
            let mut error = service_error(error);
            error.message = format!("{} (session {})", error.message, queued.id);
            error
        })?;
        terminal_response(session)
    }
}

impl<B: Backend> Backend for ExecutionBackend<B> {
    fn execute(&mut self, request: Request) -> Result<Response, CommandError> {
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
        let task_options = if operation == "prompt" {
            &request.command["arguments"]["options"]
        } else if start {
            &request.command["arguments"]["arguments"]
        } else {
            &request.command["arguments"]
        };
        if task_options
            .get("effort")
            .is_some_and(|value| !value.is_null())
        {
            return Err(unsupported("option effort"));
        }
        let grants = &task_options["tool_grants"];
        let grants = tool_policy::grants(grants)?;
        // Preserve the store-only queue contract when execution was not configured.
        if start && self.adapter.is_none() && grants.is_empty() {
            return self.fallback.execute(request);
        }
        let arguments = object(
            command
                .get("arguments")
                .ok_or_else(|| invalid("arguments are required"))?,
            "arguments",
        )?;
        let (prompt, directory) = if operation == "prompt" {
            fields(arguments, &["prompt", "options"])?;
            if arguments.get("prompt").is_none_or(Value::is_null) {
                return Err(unsupported("interactive sessions"));
            }
            let options = object(
                arguments
                    .get("options")
                    .ok_or_else(|| invalid("prompt options are required"))?,
                "options",
            )?;
            fields(
                options,
                &[
                    "cloud",
                    "on",
                    "dir",
                    "yes",
                    "detach",
                    "repo",
                    "no_repo",
                    "model",
                    "effort",
                    "resume",
                    "update",
                    "upgrade",
                    "tool_grants",
                ],
            )?;
            inactive(
                options,
                &["on", "repo", "model", "resume"],
                &["cloud", "detach", "no_repo", "update", "upgrade"],
            )?;
            if options.get("yes").is_some_and(|value| !value.is_boolean()) {
                return Err(invalid("yes must be a boolean"));
            }
            (string(arguments, "prompt")?, directory(options.get("dir"))?)
        } else {
            let args = if start {
                fields(arguments, &["operation", "arguments"])?;
                object(
                    arguments
                        .get("arguments")
                        .ok_or_else(|| invalid("start arguments are required"))?,
                    "start arguments",
                )?
            } else {
                arguments
            };
            if start {
                fields(
                    args,
                    &[
                        "prompt",
                        "repo",
                        "provider",
                        "model",
                        "effort",
                        "tool_grants",
                    ],
                )?;
                inactive(args, &["repo", "provider", "model"], &[])?;
            } else {
                fields(
                    args,
                    &[
                        "task",
                        "repo",
                        "git_ref",
                        "model",
                        "effort",
                        "detach",
                        "tool_grants",
                    ],
                )?;
                inactive(args, &["repo", "git_ref", "model"], &["detach"])?;
            }
            (
                string(args, if start { "prompt" } else { "task" })?,
                directory(None)?,
            )
        };
        self.run(
            CreateSession {
                prompt: prompt.into(),
                directory,
                model: None,
                idempotency_key: None,
            },
            grants,
        )
    }
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

fn directory(value: Option<&Value>) -> Result<String, CommandError> {
    let path = match value {
        None | Some(Value::Null) => {
            std::env::current_dir().map_err(|_| invalid("working directory is unavailable"))?
        }
        Some(Value::String(path)) if !path.trim().is_empty() => PathBuf::from(path),
        _ => return Err(invalid("working directory must be a nonempty path")),
    };
    let path = Path::new(&path)
        .canonicalize()
        .map_err(|_| invalid("working directory does not exist"))?;
    if !path.is_dir() {
        return Err(invalid("working directory must be a directory"));
    }
    path.into_os_string()
        .into_string()
        .map_err(|_| invalid("working directory must be UTF-8"))
}
fn object<'a>(value: &'a Value, label: &str) -> Result<&'a Map<String, Value>, CommandError> {
    value
        .as_object()
        .ok_or_else(|| invalid(&format!("{label} must be an object")))
}
fn fields(arguments: &Map<String, Value>, allowed: &[&str]) -> Result<(), CommandError> {
    for key in arguments.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(unsupported(&format!("option {key}")));
        }
    }
    Ok(())
}
fn string<'a>(arguments: &'a Map<String, Value>, key: &str) -> Result<&'a str, CommandError> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("{key} must be a string")))
}
fn inactive(
    arguments: &Map<String, Value>,
    optional: &[&str],
    flags: &[&str],
) -> Result<(), CommandError> {
    for key in optional {
        if arguments.get(*key).is_some_and(|value| !value.is_null()) {
            return Err(unsupported(&format!("option {key}")));
        }
    }
    for key in flags {
        match arguments.get(*key) {
            None | Some(Value::Bool(false)) => {}
            Some(Value::Bool(true)) => return Err(unsupported(&format!("option {key}"))),
            _ => return Err(invalid(&format!("{key} must be a boolean"))),
        }
    }
    Ok(())
}
fn invalid(message: &str) -> CommandError {
    CommandError::new("invalid", message)
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
