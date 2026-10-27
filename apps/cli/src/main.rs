mod startup;

use clap::{Parser, error::ErrorKind};
use f5y::{CommandError, args::Cli};
use std::{
    ffi::OsString,
    io::{self, Write},
    process::ExitCode,
};

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("error: cannot write command output: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> io::Result<u8> {
    let args: Vec<OsString> = std::env::args_os().collect();
    match Cli::try_parse_from(&args) {
        Ok(cli) => startup::run(cli),
        Err(error) => {
            // Clap's help/version output remains configuration-free and on stdout.
            if !error.use_stderr() || !json_requested(&args) {
                error.print()?;
                return Ok(error.exit_code() as u8);
            }
            let code = match error.kind() {
                ErrorKind::ValueValidation | ErrorKind::InvalidValue => "invalid",
                _ => "usage",
            };
            let error = CommandError::new(code, error.render().to_string());
            writeln!(
                io::stderr().lock(),
                "{}",
                serde_json::json!({"error": error})
            )?;
            Ok(2)
        }
    }
}

fn json_requested(args: &[OsString]) -> bool {
    // Parsing may fail before reaching a global format flag. Inspect only exact
    // supported spellings, stopping at the positional-data delimiter.
    let mut args = args
        .iter()
        .skip(1)
        .take_while(|arg| *arg != "--")
        .peekable();
    while let Some(arg) = args.next() {
        if arg == "--json" || arg == "--output-format=json" {
            return true;
        }
        if arg == "--output-format" && args.peek().is_some_and(|value| *value == "json") {
            return true;
        }
    }
    false
}
