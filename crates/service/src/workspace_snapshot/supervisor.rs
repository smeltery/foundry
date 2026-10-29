use super::{Limits, SnapshotArtifact, SnapshotFailure as Failure, filesystem};
use crate::profile_host::{Error, process};
use foundry_tools::workspace_snapshot::{self as wire, Reply, Request};
use std::{
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
    },
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

fn retained(fd: i32) -> Result<std::fs::File, Failure> {
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
    if copy < 0 {
        return Err(Failure::Io);
    }
    Ok(unsafe { std::fs::File::from_raw_fd(copy) })
}
fn worker_failure(error: Error) -> Failure {
    match error {
        Error::Invalid | Error::Policy | Error::Mapping => Failure::Invalid,
        Error::Unsupported => Failure::Unsupported,
        Error::Internal => Failure::Io,
        Error::Deadline => Failure::Limit,
        Error::Cancelled => Failure::AbortedHard,
        Error::Cleanup => Failure::Cleanup,
    }
}
pub(super) fn attempt(
    helper: &Path,
    workspace: &Path,
    destination: &Path,
    limits: Limits,
    hard_cancelled: impl Fn() -> bool,
) -> Result<SnapshotArtifact, Failure> {
    if !limits.valid() || !helper.is_absolute() || !helper.is_file() {
        return Err(Failure::Invalid);
    }
    let deadline = Instant::now() + Duration::from_millis(limits.timeout_ms);
    if hard_cancelled() {
        return Err(Failure::AbortedHard);
    }
    let root = wire::open_absolute(workspace).map_err(|_| Failure::Invalid)?;
    let parent = wire::open_absolute(destination).map_err(|_| Failure::Invalid)?;
    filesystem::private(&parent)?;
    filesystem::outside(&root, &parent)?;
    let nonce = uuid::Uuid::new_v4().to_string();
    let staging = format!(".snapshot-{nonce}");
    let final_name = format!("snapshot-{nonce}");
    let stage = filesystem::stage(&parent, &staging)?;
    let operation = (|| {
        let source_fd = retained(root.as_raw_fd())?;
        let target_fd = retained(stage.as_raw_fd())?;
        let mut command = Command::new(helper);
        command.arg("--workspace-snapshot").env_clear();
        // Only async-signal-safe descriptor operations run after fork. Reserved
        // copies above fd 9 prevent a source/target collision with fd 3 or 4.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(source_fd.as_raw_fd(), 3) < 0
                    || libc::dup2(target_fd.as_raw_fd(), 4) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let input =
            serde_json::to_vec(&Request { version: 1, limits }).map_err(|_| Failure::Invalid)?;
        let output = process::run_with_output_limit(
            &mut command,
            &input,
            deadline,
            &hard_cancelled,
            wire::MAX_REPLY_BYTES,
        )
        .map_err(worker_failure)?;
        let reply: Reply = serde_json::from_slice(&output).map_err(|_| Failure::Invalid)?;
        let (bytes, entries, manifest_digest) = match reply {
            Reply::Copied {
                bytes,
                entries,
                manifest_digest,
            } if bytes <= limits.bytes && entries <= limits.entries => {
                (bytes, entries, manifest_digest)
            }
            Reply::Copied { .. } => return Err(Failure::Invalid),
            Reply::Failed { code } => {
                return Err(match code {
                    wire::Failure::Invalid => Failure::Invalid,
                    wire::Failure::Io => Failure::Io,
                    wire::Failure::Limit => Failure::Limit,
                    wire::Failure::Changed => Failure::Changed,
                    wire::Failure::Unsupported => Failure::Unsupported,
                });
            }
        };
        // Fixed terminal decision after checked process cleanup. A hard signal
        // arriving after this check can lose to atomic artifact publication.
        if hard_cancelled() {
            return Err(Failure::AbortedHard);
        }
        if Instant::now() >= deadline {
            return Err(Failure::Limit);
        }
        filesystem::outside(&root, &parent)?;
        filesystem::publish(&parent, &staging, &final_name)?;
        Ok(SnapshotArtifact {
            directory: destination.join(&final_name),
            bytes,
            entries,
            manifest_digest,
        })
    })();
    if operation.is_err() {
        // Never delete files while worker cleanup is unverified. The private
        // staging directory then remains for explicit owner recovery.
        if matches!(operation, Err(Failure::Cleanup)) {
            return operation;
        }
        filesystem::discard(&parent, &staging)?;
    }
    operation
}
