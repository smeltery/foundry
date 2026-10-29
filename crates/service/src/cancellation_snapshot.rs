//! Owner-polled filesystem snapshot work, separate from conversational checkpoints.
use crate::{
    Result, ServiceError,
    workspace_snapshot::{self, Limits, SnapshotArtifact, SnapshotFailure},
};
use foundry_store::{AttemptFence, Store};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

#[derive(Clone)]
pub struct SnapshotSettings {
    pub helper: PathBuf,
    pub destination: PathBuf,
    pub limits: Limits,
}

impl SnapshotSettings {
    /// The database parent owns a private artifact directory. A destination inside
    /// the workspace is rejected by the copy operation, never silently excluded.
    pub fn for_database(helper: PathBuf, database: &Path) -> Result<Self> {
        let database = std::path::absolute(database)
            .map_err(|_| invalid("snapshot database path unavailable"))?;
        let parent = database
            .parent()
            .ok_or_else(|| invalid("snapshot parent unavailable"))?;
        let parent = parent
            .canonicalize()
            .map_err(|_| invalid("snapshot parent unavailable"))?;
        let destination = parent.join(".f5y-workspace-snapshots");
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt};
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(&destination) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(invalid("snapshot destination unavailable")),
            }
            let metadata = std::fs::symlink_metadata(&destination)
                .map_err(|_| invalid("snapshot destination unavailable"))?;
            if !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o7777 != 0o700
            {
                return Err(invalid("snapshot destination must be private"));
            }
        }
        #[cfg(not(unix))]
        return Err(invalid("workspace snapshots require Unix"));
        #[cfg(unix)]
        Ok(Self {
            helper,
            destination,
            limits: Limits {
                bytes: 1024 * 1024 * 1024,
                entries: 100_000,
                depth: 128,
                timeout_ms: 30_000,
            },
        })
    }
}

pub(crate) type SnapshotResult = std::result::Result<SnapshotArtifact, SnapshotFailure>;

/// Poll alongside the real owner's heartbeat. Drop aborts and joins its worker.
pub(crate) struct SnapshotTask {
    abort: Arc<AtomicBool>,
    worker: Option<JoinHandle<SnapshotResult>>,
}
impl SnapshotTask {
    pub(crate) fn start(
        settings: SnapshotSettings,
        workspace: PathBuf,
        should_abort: impl Fn() -> bool + Send + 'static,
    ) -> Result<Self> {
        let abort = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&abort);
        let worker = thread::Builder::new()
            .name("foundry-workspace-snapshot".into())
            .spawn(move || {
                workspace_snapshot::attempt(
                    &settings.helper,
                    &workspace,
                    &settings.destination,
                    settings.limits,
                    || stop.load(Ordering::Acquire) || should_abort(),
                )
            })
            .map_err(|_| invalid("snapshot worker unavailable"))?;
        Ok(Self {
            abort,
            worker: Some(worker),
        })
    }

    pub(crate) fn poll(&mut self, hard: bool) -> Result<Option<SnapshotResult>> {
        if hard {
            self.abort.store(true, Ordering::Release);
        }
        if !self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            return Ok(None);
        }
        let worker = self
            .worker
            .take()
            .ok_or_else(|| invalid("snapshot worker absent"))?;
        let outcome = worker
            .join()
            .map_err(|_| invalid("snapshot worker panicked"))?;
        if matches!(outcome, Err(SnapshotFailure::Cleanup)) {
            return Err(invalid("snapshot cleanup unverified"));
        }
        Ok(Some(outcome))
    }
}
impl Drop for SnapshotTask {
    fn drop(&mut self) {
        self.abort.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Fenced audit association only; this event is not a selected restoration head.
pub(crate) fn record(
    store: &mut Store,
    fence: &AttemptFence,
    result: SnapshotResult,
) -> Result<()> {
    let data = match result {
        Ok(artifact) => {
            let name = artifact
                .directory
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| invalid("snapshot artifact identity unavailable"))?;
            let digest: String = artifact
                .manifest_digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            json!({"status":"captured","artifact":name,"manifest_digest":digest,
                "bytes":artifact.bytes,"entries":artifact.entries,"atomic":false})
        }
        Err(SnapshotFailure::Cleanup) => return Err(invalid("snapshot cleanup unverified")),
        Err(failure) => json!({"status":"failed","code": match failure {
            SnapshotFailure::Invalid => "invalid", SnapshotFailure::Io => "io",
            SnapshotFailure::Limit => "limit", SnapshotFailure::Changed => "changed",
            SnapshotFailure::Unsupported => "unsupported", SnapshotFailure::AbortedHard => "aborted_hard",
            SnapshotFailure::Cleanup => return Err(invalid("snapshot cleanup unverified")),
        }}),
    };
    store.append_event(
        fence,
        &format!("workspace-snapshot:{}", fence.attempt_id),
        0,
        "workspace_snapshot",
        data,
    )?;
    Ok(())
}

fn invalid(message: &'static str) -> ServiceError {
    ServiceError::Coordination(message)
}

impl crate::Service {
    pub(crate) fn finish_clean_run(
        &mut self,
        fence: &AttemptFence,
        session: &foundry_contracts::Session,
        runtime: foundry_runtime::RunResult,
        requested: &Arc<std::sync::atomic::AtomicU8>,
        lease: &crate::run_lease::RunLease,
    ) -> Result<foundry_contracts::Session> {
        use foundry_contracts::{ContractError, Lifecycle};
        use foundry_runtime::RunResult;
        if let RunResult::Cancelled { mode, .. } = &runtime {
            self.store.request_cancel(&session.id, *mode)?;
        }
        let expired = lease.expired();
        loop {
            let snapshot_recorded = self.snapshot_cancelled(fence, session, requested, &expired)?;
            let cancelled = self.store.cancellation(fence)?.is_some();
            if cancelled && !snapshot_recorded {
                continue;
            }
            let (lifecycle, outcome, text) = match &runtime {
                RunResult::Completed { outcome, result } if !cancelled => {
                    (Lifecycle::Exit, Some(*outcome), Some(result.clone()))
                }
                _ => (Lifecycle::Suspended, None, None),
            };
            let result =
                lease.settle(|| Ok(self.store.finish_attempt(fence, lifecycle, outcome, text)?));
            match result {
                Err(ServiceError::Store(foundry_store::StoreError::Contract(
                    ContractError::Conflict(_),
                ))) if lifecycle != Lifecycle::Suspended
                    && self.store.cancellation(fence)?.is_some() =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }

    pub(crate) fn snapshot_cancelled(
        &mut self,
        fence: &AttemptFence,
        session: &foundry_contracts::Session,
        requested: &Arc<std::sync::atomic::AtomicU8>,
        expired: &Arc<AtomicBool>,
    ) -> Result<bool> {
        let Some(settings) = self.snapshots.clone() else {
            return Ok(true);
        };
        let local = requested.load(Ordering::Acquire);
        if local > 0 {
            self.store.request_cancel(
                &session.id,
                if local >= 2 {
                    foundry_contracts::CancelMode::Hard
                } else {
                    foundry_contracts::CancelMode::Graceful
                },
            )?;
        }
        let Some(mode) = self.store.cancellation(fence)? else {
            return Ok(false);
        };
        if mode == foundry_contracts::CancelMode::Hard {
            return record(&mut self.store, fence, Err(SnapshotFailure::AbortedHard)).map(|_| true);
        }
        let local = Arc::clone(requested);
        let lost = Arc::clone(expired);
        let mut task =
            SnapshotTask::start(settings, session.directory.clone().into(), move || {
                local.load(Ordering::Acquire) >= 2 || lost.load(Ordering::Acquire)
            })?;
        loop {
            if expired.load(Ordering::Acquire) {
                return Err(invalid("snapshot lease expired"));
            }
            let durable = self.store.cancellation(fence)?;
            let hard = durable == Some(foundry_contracts::CancelMode::Hard)
                || requested.load(Ordering::Acquire) >= 2;
            if let Some(result) = task.poll(hard)? {
                if expired.load(Ordering::Acquire) {
                    return Err(invalid("snapshot lease expired"));
                }
                return record(&mut self.store, fence, result).map(|_| true);
            }
            thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
