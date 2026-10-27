use f5y::{
    CommandError, UnavailableBackend,
    args::{Cli, Command, OutputFormat, PromptOptions},
    execute,
    local::LocalBackend,
    runner::ExecutionBackend,
    session::SessionCommand,
    watch::{self, WatchFormat, WatchOptions},
};
use serde_json::json;
use std::{
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
    time::Duration,
};

#[path = "interrupt.rs"]
mod interrupt;
use interrupt::SignalControl;

fn database_path(
    explicit: Option<PathBuf>,
    environment: Option<OsString>,
) -> Result<PathBuf, CommandError> {
    let path = explicit.or_else(|| environment.map(PathBuf::from))
        .ok_or_else(|| CommandError::new("configuration",
            "Select a database with --database PATH or FOUNDRY_DATABASE; no default database is created"))?;
    if path.as_os_str().is_empty() {
        return Err(CommandError::new(
            "configuration",
            "Database path must not be empty",
        ));
    }
    Ok(path)
}

/// Help and version bypass database and adapter configuration.
pub fn run(mut cli: Cli) -> io::Result<u8> {
    if let Err(error) = f5y::normalize_upgrade(&mut cli) {
        return report_error(cli.output.format(), error);
    }
    if matches!(cli.command, Some(Command::Version | Command::Help { .. })) {
        return execute(
            cli,
            &mut UnavailableBackend,
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        );
    }
    if cli.command.is_some()
        && (cli.prompt.is_some()
            || serde_json::to_value(&cli.options)?
                != serde_json::to_value(PromptOptions::default())?)
    {
        return report_error(
            cli.output.format(),
            CommandError::new(
                "usage",
                "Root task options cannot be combined with a subcommand",
            ),
        );
    }
    let path = match database_path(cli.database.take(), std::env::var_os("FOUNDRY_DATABASE")) {
        Ok(path) => path,
        Err(error) => return report_error(cli.output.format(), error),
    };
    if let Some(Command::Watch(options) | Command::Session(SessionCommand::Watch(options))) =
        &cli.command
    {
        let format = cli.output.format();
        let id = &options.id;
        let options = WatchOptions {
            interval: Duration::from_secs(options.interval.unwrap_or(1)),
            format: match format {
                OutputFormat::Text => WatchFormat::Text,
                OutputFormat::Json => WatchFormat::JsonLines,
            },
            ..WatchOptions::default()
        };
        let mut signals = SignalControl::install()?;
        let result = watch::watch_with_control(
            &path,
            id,
            options,
            &signals.control(),
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        );
        signals.shutdown()?;
        return match result {
            Ok(code) => Ok(code),
            Err(error) => report_error(format, CommandError::new("watch", error.to_string())),
        };
    }
    let mut backend = LocalBackend::new(&path);
    if matches!(cli.command, Some(Command::Mcp)) {
        // The transport owns both streams; do not hold an outer stdout lock.
        return Ok(if foundry_mcp::serve_stdio(&mut backend).is_ok() {
            0
        } else {
            2
        });
    }
    let mut backend = match ExecutionBackend::from_env(path, backend) {
        Ok(backend) => backend,
        Err(error) => return report_error(cli.output.format(), error),
    };
    let executing = matches!(
        cli.command,
        Some(Command::Run(_) | Command::Session(SessionCommand::Start { .. }))
    ) || (cli.command.is_none() && cli.prompt.is_some());
    let mut signals = if executing {
        Some(SignalControl::install()?)
    } else {
        None
    };
    if let Some(signals) = &signals {
        backend = backend.with_control(signals.control());
    }
    let result = execute(
        cli,
        &mut backend,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    );
    if let Some(signals) = &mut signals {
        signals.shutdown()?;
    }
    result
}

fn report_error(format: OutputFormat, error: CommandError) -> io::Result<u8> {
    let mut stderr = io::stderr().lock();
    match format {
        OutputFormat::Text => writeln!(stderr, "error: {}", error.message)?,
        OutputFormat::Json => writeln!(stderr, "{}", json!({"error": error}))?,
    }
    Ok(2)
}
