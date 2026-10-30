#[cfg(unix)]
#[tokio::main]
async fn main() -> std::process::ExitCode {
    use foundry_network::{daemon_config::DaemonConfig, daemon_host::Daemon};
    use std::{
        io::{self, Write},
        path::PathBuf,
        process::ExitCode,
    };
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!(
            "Usage: f5y-daemon --config PATH\nRun the local foreground daemon using explicit private state and socket directories."
        );
        return ExitCode::SUCCESS;
    }
    if args.len() != 2 || args[0] != "--config" {
        eprintln!("Usage: f5y-daemon --config PATH");
        return ExitCode::from(2);
    }
    let config = match DaemonConfig::load(&PathBuf::from(&args[1])) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };
    let daemon = match Daemon::open(config).await {
        Ok(daemon) => daemon,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let ready = serde_json::json!({"event":"ready","socket":daemon.path()});
    let mut stdout = io::stdout().lock();
    if serde_json::to_writer(&mut stdout, &ready).is_err()
        || stdout.write_all(b"\n").is_err()
        || stdout.flush().is_err()
    {
        eprintln!("error: daemon readiness output failed");
        return ExitCode::FAILURE;
    }
    drop(stdout);
    match daemon.serve().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("error: foreground daemon ownership is unsupported on this platform");
    std::process::ExitCode::FAILURE
}
