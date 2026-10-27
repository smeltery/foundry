pub mod account;
pub mod args;
pub mod computer;
pub mod help;
pub mod local;
pub mod runner;
pub mod session;
pub mod watch;

use args::{Cli, Command, OutputFormat};
use serde::Serialize;
use serde_json::{Value, json};
use std::io::Write;

/// Versioned, transport-independent request for a backend adapter.
#[derive(Debug, Serialize)]
pub struct Request {
    pub version: u8,
    pub command: Value,
}

pub struct Response {
    pub text: String,
    pub json: Value,
    pub exit_code: u8,
}

#[derive(Debug, Serialize)]
pub struct CommandError {
    pub code: String,
    pub message: String,
}

impl CommandError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

pub trait Backend {
    fn execute(&mut self, request: Request) -> Result<Response, CommandError>;
}

pub struct UnavailableBackend;

impl Backend for UnavailableBackend {
    fn execute(&mut self, request: Request) -> Result<Response, CommandError> {
        let operation = request.command["operation"].as_str().unwrap_or("unknown");
        Err(CommandError::new(
            "unsupported",
            format!("Foundry backend does not implement '{operation}' yet"),
        ))
    }
}

/// Normalize the standalone update flag before selecting an execution backend.
pub fn normalize_upgrade(cli: &mut Cli) -> Result<(), CommandError> {
    if !cli.tool_grants.is_empty()
        && (cli.command.is_some() || cli.prompt.is_none() || cli.options.upgrade)
    {
        return Err(CommandError::new(
            "usage",
            "Root tool grants require a task prompt",
        ));
    }
    if cli.options.upgrade {
        cli.options.upgrade = false;
        if cli.command.is_some()
            || cli.prompt.is_some()
            || serde_json::to_value(&cli.options)
                .map_err(|error| CommandError::new("encoding", error.to_string()))?
                != serde_json::to_value(args::PromptOptions::default())
                    .map_err(|error| CommandError::new("encoding", error.to_string()))?
        {
            return Err(CommandError::new(
                "usage",
                "--upgrade cannot be combined with task options or a subcommand",
            ));
        }
        cli.command = Some(Command::Update(account::UpdateOptions::default()));
    }
    Ok(())
}

pub fn dispatch(mut cli: Cli, backend: &mut impl Backend) -> Result<Response, CommandError> {
    normalize_upgrade(&mut cli)?;
    if let Some(Command::Help { command }) = &cli.command {
        return help::render(command);
    }
    if matches!(cli.command, Some(Command::Version)) {
        let version = env!("FOUNDRY_VERSION");
        return Ok(Response {
            text: format!("f5y {version}"),
            json: json!({"name": "f5y", "version": version}),
            exit_code: 0,
        });
    }
    let command = match cli.command {
        Some(command) => serde_json::to_value(command)
            .map_err(|error| CommandError::new("encoding", error.to_string()))?,
        None => {
            if cli.options.detach && !cli.options.cloud && cli.options.on.is_none() {
                return Err(CommandError::new("usage", "--detach needs --cloud or --on"));
            }
            if cli
                .prompt
                .as_ref()
                .is_some_and(|prompt| prompt.trim().is_empty())
            {
                return Err(CommandError::new("usage", "task must not be empty"));
            }
            let mut options = serde_json::to_value(cli.options)
                .map_err(|error| CommandError::new("encoding", error.to_string()))?;
            if !cli.tool_grants.is_empty() {
                options["tool_grants"] = json!(cli.tool_grants);
            }
            json!({"operation": "prompt", "arguments": {
                "prompt": cli.prompt, "options": options
            }})
        }
    };
    backend.execute(Request {
        version: 1,
        command,
    })
}

/// Errors go to stderr in both formats; stdout is reserved for successful results.
pub fn execute(
    cli: Cli,
    backend: &mut impl Backend,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> std::io::Result<u8> {
    let format = cli.output.format();
    match dispatch(cli, backend) {
        Ok(response) => {
            match format {
                OutputFormat::Text => writeln!(stdout, "{}", response.text)?,
                OutputFormat::Json => writeln!(stdout, "{}", response.json)?,
            }
            Ok(response.exit_code)
        }
        Err(error) => {
            match format {
                OutputFormat::Text => writeln!(stderr, "error: {}", error.message)?,
                OutputFormat::Json => writeln!(stderr, "{}", json!({"error": error}))?,
            }
            Ok(2)
        }
    }
}
