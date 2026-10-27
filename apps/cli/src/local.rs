//! Store-only session operations. Creating a record does not start execution.
use crate::{Backend, CommandError, Request, Response};
use foundry_contracts::{CancelMode, ContractError, CreateSession, MAX_PROMPT_BYTES, MessageState};
use foundry_store::{Store, StoreError};
use serde_json::{Map, Value, json};
use std::path::PathBuf;

pub struct LocalBackend {
    path: PathBuf,
    store: Option<Store>,
}

impl LocalBackend {
    /// Select an explicit database path; opening is deferred until a valid operation.
    /// Parent directories must already exist. No runtime is started.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            store: None,
        }
    }

    fn store(&mut self) -> Result<&mut Store, CommandError> {
        if self.store.is_none() {
            self.store = Some(Store::open(&self.path).map_err(store_error)?);
        }
        Ok(self.store.as_mut().unwrap())
    }

    pub fn create(&mut self, request: CreateSession) -> Result<Value, CommandError> {
        request.validate().map_err(contract_error)?;
        if request.model.is_some() {
            return Err(unsupported(
                "model configuration requires runtime integration",
            ));
        }
        let session = self.store()?.create(request).map_err(store_error)?;
        Ok(json!({"session":session,"execution":"not_started","target":"local"}))
    }

    pub fn list(&mut self, limit: u32) -> Result<Value, CommandError> {
        if limit == 0 {
            return Err(invalid("limit must be positive"));
        }
        let sessions = self.store()?.list(limit).map_err(store_error)?;
        Ok(json!({"sessions":sessions,"limit":limit}))
    }

    pub fn get(&mut self, id: &str) -> Result<Value, CommandError> {
        nonempty(id)?;
        Ok(json!({"session":self.store()?.get(id).map_err(store_error)?}))
    }

    /// A durable cancellation request is not proof that a running process stopped.
    pub fn cancel(&mut self, id: &str, mode: CancelMode) -> Result<Value, CommandError> {
        nonempty(id)?;
        Ok(
            json!({"session":self.store()?.request_cancel(id, mode).map_err(store_error)?,
            "cancellation":"requested"}),
        )
    }

    /// Enqueue only. Retries preserve pending/in-flight/delivered/withdrawn state;
    /// adapter acknowledgment does not establish that an instruction completed.
    pub fn send(&mut self, id: &str, message_id: &str, text: &str) -> Result<Value, CommandError> {
        nonempty(id)?;
        nonempty(message_id)?;
        nonempty(text)?;
        if text.len() > MAX_PROMPT_BYTES {
            return Err(invalid("message exceeds 262144 UTF-8 bytes"));
        }
        let message = self
            .store()?
            .enqueue_message(id, message_id, text)
            .map_err(store_error)?;
        let delivery_not_confirmed = message.state != MessageState::Delivered;
        Ok(
            json!({"message":message,"delivery_not_confirmed":delivery_not_confirmed,
            "completion":"not_confirmed"}),
        )
    }

    /// Read a session once; this does not subscribe, poll, or claim completion.
    pub fn watch_snapshot(&mut self, id: &str) -> Result<Value, CommandError> {
        let mut value = self.get(id)?;
        value["watch"] = json!("snapshot");
        Ok(value)
    }

    fn command(&mut self, operation: &str, args: &Value) -> Result<Value, CommandError> {
        let args = args
            .as_object()
            .ok_or_else(|| invalid("arguments must be an object"))?;
        match operation {
            "start" => {
                fields(args, &["prompt"], &["repo", "provider", "model"])?;
                let prompt = string(args, "prompt")?.to_owned();
                nonempty(&prompt)?;
                let directory = std::env::current_dir()
                    .map_err(|error| CommandError::new("io", error.to_string()))?;
                let directory = directory
                    .into_os_string()
                    .into_string()
                    .map_err(|_| invalid("working directory must be UTF-8"))?;
                self.create(CreateSession {
                    prompt,
                    directory,
                    model: None,
                    idempotency_key: None,
                })
            }
            "list" | "ls" => {
                fields(args, &["limit"], &["status", "origin", "fields"])?;
                let limit = match args.get("limit").filter(|value| !value.is_null()) {
                    Some(value) => value
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                        .ok_or_else(|| invalid("limit must be a positive 32-bit integer"))?,
                    None => 100,
                };
                self.list(limit)
            }
            "get" => {
                fields(args, &["id"], &[])?;
                self.get(string(args, "id")?)
            }
            "send" => {
                fields(args, &["id", "message"], &[])?;
                self.send(
                    string(args, "id")?,
                    &uuid::Uuid::new_v4().to_string(),
                    string(args, "message")?,
                )
            }
            "watch" => Err(unsupported("watch requires a live follow loop")),
            "cancel" => {
                fields(args, &["id", "hard"], &[])?;
                let mode = match args.get("hard") {
                    None | Some(Value::Bool(false)) => CancelMode::Graceful,
                    Some(Value::Bool(true)) => CancelMode::Hard,
                    _ => return Err(invalid("hard must be a boolean")),
                };
                self.cancel(string(args, "id")?, mode)
            }
            _ => Err(unsupported(operation)),
        }
    }
}

impl Backend for LocalBackend {
    fn execute(&mut self, request: Request) -> Result<Response, CommandError> {
        if request.version != 1 {
            return Err(unsupported("request version"));
        }
        let command = request
            .command
            .as_object()
            .ok_or_else(|| invalid("command must be an object"))?;
        fields(command, &["operation", "arguments"], &[])?;
        let operation = string(command, "operation")?;
        let empty = json!({});
        let args = command.get("arguments").unwrap_or(&empty);
        let value = if operation == "session" {
            let nested = args
                .as_object()
                .ok_or_else(|| invalid("session command must be an object"))?;
            fields(nested, &["operation", "arguments"], &[])?;
            self.command(
                string(nested, "operation")?,
                nested.get("arguments").unwrap_or(&empty),
            )?
        } else {
            match operation {
                "ls" | "watch" | "cancel" | "send" => self.command(operation, args)?,
                _ => return Err(unsupported(operation)),
            }
        };
        Ok(Response {
            text: value.to_string(),
            json: value,
            exit_code: 0,
        })
    }
}

impl foundry_mcp::Backend for LocalBackend {
    fn call(&mut self, name: &str, args: Value) -> Result<Value, foundry_mcp::McpError> {
        self.mcp_call(name, args)
            .map_err(|error| foundry_mcp::McpError::new(error.code, error.message))
    }
}

impl LocalBackend {
    fn mcp_call(&mut self, name: &str, args: Value) -> Result<Value, CommandError> {
        let args = args
            .as_object()
            .ok_or_else(|| invalid("arguments must be an object"))?;
        match name {
            "session_create" => {
                fields(args, &["prompt", "directory", "idempotency_key"], &[])?;
                let key = args
                    .get("idempotency_key")
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| invalid("idempotency_key must be a string"))
                    })
                    .transpose()?;
                self.create(CreateSession {
                    prompt: string(args, "prompt")?.into(),
                    directory: string(args, "directory")?.into(),
                    model: None,
                    idempotency_key: key,
                })
            }
            "session_list" => {
                fields(args, &[], &[])?;
                self.list(100)
            }
            "session_get" => {
                fields(args, &["session_id"], &[])?;
                self.get(string(args, "session_id")?)
            }
            "session_send" => {
                fields(args, &["session_id", "message_id", "message"], &[])?;
                self.send(
                    string(args, "session_id")?,
                    string(args, "message_id")?,
                    string(args, "message")?,
                )
            }
            "session_cancel" => {
                fields(args, &["session_id", "mode"], &[])?;
                let mode = match string(args, "mode")? {
                    "graceful" => CancelMode::Graceful,
                    "hard" => CancelMode::Hard,
                    _ => return Err(invalid("mode must be graceful or hard")),
                };
                self.cancel(string(args, "session_id")?, mode)
            }
            _ => Err(unsupported(name)),
        }
    }
}

fn fields(
    args: &Map<String, Value>,
    supported: &[&str],
    inactive: &[&str],
) -> Result<(), CommandError> {
    for (key, value) in args {
        if supported.contains(&key.as_str()) {
            continue;
        }
        if inactive.contains(&key.as_str())
            && (value.is_null() || (key == "fields" && value.as_array().is_some_and(Vec::is_empty)))
        {
            continue;
        }
        return Err(unsupported(&format!("option {key}")));
    }
    Ok(())
}
fn string<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, CommandError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("{key} must be a string")))
}
fn nonempty(value: &str) -> Result<(), CommandError> {
    if value.trim().is_empty() {
        Err(invalid("value must not be blank"))
    } else {
        Ok(())
    }
}
fn invalid(message: &str) -> CommandError {
    CommandError::new("invalid", message)
}
fn unsupported(operation: &str) -> CommandError {
    CommandError::new(
        "unsupported",
        format!("Local store backend does not support {operation}"),
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
fn store_error(error: StoreError) -> CommandError {
    match error {
        StoreError::Contract(error) => contract_error(error),
        other => CommandError::new("storage", other.to_string()),
    }
}
