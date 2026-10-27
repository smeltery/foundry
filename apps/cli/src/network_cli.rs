//! Dispatch and rendering only; service, onboarding and protocol execution are backend-owned.
use crate::{
    Backend, CommandError, Request, Response,
    args::{OutputFormat, OutputOptions},
    network_args::{NetworkCli, NetworkCommand, NetworkOptions, StandaloneCommand},
};
use serde_json::json;
use std::io::{self, Write};

pub fn local_json(options: &NetworkOptions) -> bool {
    matches!(&options.command, Some(NetworkCommand::Standalone(command)) if command.json())
}

pub fn normalize(
    options: &mut NetworkOptions,
    output: &mut OutputOptions,
) -> Result<(), CommandError> {
    if options.address.as_deref() == Some("__relay") {
        return Err(CommandError::new(
            "unsupported",
            "Private network relay schema is unavailable",
        ));
    }
    let local = local_json(options);
    if local && matches!(output.output_format, Some(OutputFormat::Text)) {
        return Err(CommandError::new(
            "usage",
            "Network JSON conflicts with text output",
        ));
    }
    let json = local || matches!(output.format(), OutputFormat::Json);
    if let Some(NetworkCommand::Standalone(command)) = &mut options.command {
        if json {
            *command.json_mut().ok_or_else(|| {
                CommandError::new(
                    "unsupported_format",
                    "This network command does not support JSON output",
                )
            })? = true;
        }
        validate(command)?;
    } else if json {
        return Err(CommandError::new(
            "unsupported_format",
            "This network route does not support JSON output",
        ));
    }
    if json {
        output.json = true;
    }
    Ok(())
}
fn validate(command: &StandaloneCommand) -> Result<(), CommandError> {
    match command {
        StandaloneCommand::Approve { ids, all } | StandaloneCommand::Reject { ids, all }
            if ids.is_empty() && !all =>
        {
            Err(CommandError::new("usage", "Select message IDs or --all"))
        }
        StandaloneCommand::Send { message, .. }
            if !(message.is_empty() || message.len() == 1 && message[0] == "-")
                && message.join(" ").trim().is_empty() =>
        {
            Err(CommandError::new("usage", "Message must not be blank"))
        }
        _ => Ok(()),
    }
}
fn options(cli: NetworkCli) -> NetworkOptions {
    NetworkOptions {
        command: cli.command.map(NetworkCommand::Standalone),
        address: None,
        message: None,
    }
}
pub fn dispatch(cli: NetworkCli, backend: &mut impl Backend) -> Result<Response, CommandError> {
    let mut options = options(cli);
    normalize(&mut options, &mut OutputOptions::default())?;
    let arguments = serde_json::to_value(options)
        .map_err(|_| CommandError::new("encoding", "Cannot encode network request"))?;
    backend.execute(Request {
        version: 1,
        command: json!({"operation":"network", "arguments":arguments}),
    })
}
pub fn execute(
    cli: NetworkCli,
    backend: &mut impl Backend,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    let format = if cli.command.as_ref().is_some_and(StandaloneCommand::json) {
        OutputFormat::Json
    } else {
        OutputFormat::Text
    };
    crate::render(dispatch(cli, backend), format, stdout, stderr)
}
