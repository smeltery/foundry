//! Disposable trusted filesystem helper. Authority is inherited on fd 3.
use foundry_tools::host::HostError;
#[cfg(unix)]
use foundry_tools::host::serve;

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", error.code());
            std::process::ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), HostError> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() == 1 && arguments[0] == "--version" {
        println!("f5y-toolhost {}", env!("FOUNDRY_VERSION"));
        return Ok(());
    }
    if arguments.len() == 1 && arguments[0] == "--workspace-snapshot" {
        // Dedicated single-threaded helper adopts the two reserved descriptors once.
        return unsafe { foundry_tools::workspace_snapshot::serve_inherited() };
    }
    if !arguments.is_empty() {
        return Err(HostError::Protocol);
    }
    #[cfg(unix)]
    {
        // Fresh single-threaded process: fd 3 is reserved by the trusted launcher;
        // this is its only adoption, before any other descriptor-owning operation.
        let root = unsafe { foundry_tools::host::inherited_directory()? };
        serve(
            root,
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
        )
    }
    #[cfg(not(unix))]
    {
        Err(HostError::Unsupported)
    }
}
