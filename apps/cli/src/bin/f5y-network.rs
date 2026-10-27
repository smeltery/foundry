use clap::Parser;
use f5y::{UnavailableBackend, network_args::NetworkCli, network_cli};
use std::{io, process::ExitCode};

fn main() -> ExitCode {
    let cli = match NetworkCli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code() as u8;
            return if error.print().is_ok() {
                ExitCode::from(code)
            } else {
                ExitCode::from(2)
            };
        }
    };
    match network_cli::execute(
        cli,
        &mut UnavailableBackend,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    ) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("error: cannot write network output: {error}");
            ExitCode::from(2)
        }
    }
}
