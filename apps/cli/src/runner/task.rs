//! Resolve only local task inputs; private configuration never enters Request.
use super::{invalid, tool_policy, unsupported};
use crate::{CommandError, Request};
use foundry_tools::host::Grant;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

pub(super) struct LocalTask {
    pub prompt: String,
    pub directory: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub grants: Vec<Grant>,
}

pub(super) fn resolve(request: &Request, configured: bool) -> Result<LocalTask, CommandError> {
    let command = object(&request.command, "command")?;
    let operation = string(command, "operation")?;
    let start = operation == "session";
    let options = options(request, operation, start);
    if !configured {
        inactive(object(options, "task options")?, &["model", "effort"], &[])?;
    }
    let model = optional_string(&options["model"])?;
    let effort = optional_string(&options["effort"])?;
    if effort
        .as_deref()
        .is_some_and(|v| !matches!(v, "low" | "medium" | "high" | "xhigh"))
    {
        return Err(invalid("invalid task effort"));
    }
    let grants = tool_policy::grants(&options["tool_grants"])?;
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
            &["on", "repo", "resume"],
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
            inactive(args, &["repo", "provider"], &[])?;
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
            inactive(args, &["repo", "git_ref"], &["detach"])?;
        }
        (
            string(args, if start { "prompt" } else { "task" })?,
            directory(None)?,
        )
    };
    Ok(LocalTask {
        prompt: prompt.into(),
        directory,
        model,
        effort,
        grants,
    })
}

pub(super) fn options<'a>(request: &'a Request, operation: &str, start: bool) -> &'a Value {
    if operation == "prompt" {
        &request.command["arguments"]["options"]
    } else if start {
        &request.command["arguments"]["arguments"]
    } else {
        &request.command["arguments"]
    }
}
fn optional_string(value: &Value) -> Result<Option<String>, CommandError> {
    match value {
        Value::Null => Ok(None),
        Value::String(value) if !value.trim().is_empty() => Ok(Some(value.clone())),
        _ => Err(invalid("task selection must be a nonempty string")),
    }
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
pub(super) fn object<'a>(
    value: &'a Value,
    label: &str,
) -> Result<&'a Map<String, Value>, CommandError> {
    value
        .as_object()
        .ok_or_else(|| invalid(&format!("{label} must be an object")))
}
pub(super) fn fields(arguments: &Map<String, Value>, allowed: &[&str]) -> Result<(), CommandError> {
    for key in arguments.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(unsupported(&format!("option {key}")));
        }
    }
    Ok(())
}
pub(super) fn string<'a>(
    arguments: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a str, CommandError> {
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
