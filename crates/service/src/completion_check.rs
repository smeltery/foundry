//! Bounded v1 completion checks run by the runtime, never by the adapter.
//!
//! A command runs without a shell in the session workspace with an empty
//! environment apart from `PATH`, no input and discarded output, in its own
//! process group that is killed at the deadline. A file check opens the path
//! through a capability handle on the workspace, so absolute paths, `..` and
//! symlinks that leave the workspace are refused. Running a check grants no
//! tool permission and is not visible to the model.
use cap_std::{ambient_authority, fs::Dir};
use foundry_contracts::CompletionCheck;
use foundry_store::{CriterionResult, CriterionStatus};
use sha2::{Digest, Sha256};
use std::{
    io::{ErrorKind, Read},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const POLL: Duration = Duration::from_millis(5);

pub(crate) fn run(
    workspace: &Path,
    check: &CompletionCheck,
    stop: &mut dyn FnMut() -> bool,
) -> CriterionResult {
    let (status, detail) = match check {
        CompletionCheck::Command {
            argv, timeout_ms, ..
        } => command(workspace, argv, Duration::from_millis(*timeout_ms), stop),
        CompletionCheck::File { path, sha256, .. } => file(workspace, path, sha256.as_deref()),
    };
    CriterionResult {
        id: check.id().to_owned(),
        status,
        detail,
    }
}

fn command(
    workspace: &Path,
    argv: &[String],
    timeout: Duration,
    stop: &mut dyn FnMut() -> bool,
) -> (CriterionStatus, &'static str) {
    let Some((program, arguments)) = argv.split_first() else {
        return (CriterionStatus::Unverified, "invalid_check");
    };
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(workspace)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let started = Instant::now();
    let Ok(mut child) = command.spawn() else {
        return (CriterionStatus::Unverified, "spawn_failed");
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return (CriterionStatus::Passed, "exit_zero"),
            Ok(Some(_)) => return (CriterionStatus::Failed, "exit_nonzero"),
            Ok(None) => {}
            Err(_) => {
                terminate(&mut child);
                return (CriterionStatus::Unverified, "wait_failed");
            }
        }
        if started.elapsed() >= timeout {
            terminate(&mut child);
            return (CriterionStatus::Failed, "timed_out");
        }
        if stop() {
            terminate(&mut child);
            return (CriterionStatus::Unverified, "cancelled");
        }
        thread::sleep(POLL);
    }
}

/// Kill the whole group so a check cannot leave descendants running.
fn terminate(child: &mut Child) {
    #[cfg(unix)]
    if let Ok(group) = i32::try_from(child.id()) {
        // SAFETY: signals only the process group created for this child.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn file(workspace: &Path, path: &str, sha256: Option<&str>) -> (CriterionStatus, &'static str) {
    let Ok(root) = Dir::open_ambient_dir(workspace, ambient_authority()) else {
        return (CriterionStatus::Unverified, "workspace_unavailable");
    };
    let mut file = match root.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return (CriterionStatus::Failed, "missing");
        }
        // Includes every attempt to resolve outside the workspace.
        Err(_) => return (CriterionStatus::Unverified, "rejected"),
    };
    match file.metadata() {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return (CriterionStatus::Failed, "not_a_file"),
        Err(_) => return (CriterionStatus::Unverified, "io_error"),
    }
    let Some(expected) = sha256 else {
        return (CriterionStatus::Passed, "exists");
    };
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hash.update(&buffer[..read]),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return (CriterionStatus::Unverified, "io_error"),
        }
    }
    let actual: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual == expected {
        (CriterionStatus::Passed, "digest_match")
    } else {
        (CriterionStatus::Failed, "digest_mismatch")
    }
}
