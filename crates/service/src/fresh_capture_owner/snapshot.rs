//! Native-owner snapshot wait: keep the real attempt lease, never a context head.
use crate::{
    Result, ServiceError,
    cancellation_snapshot::{self, SnapshotSettings, SnapshotTask},
    control::RunControl,
    restoration_owner::RestoreOwnerOptions,
};
use foundry_contracts::CancelMode;
use foundry_store::{AttemptFence, Store};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub(crate) fn validate(settings: Option<&SnapshotSettings>) -> Result<()> {
    if settings.is_some_and(|s| {
        !s.helper.is_absolute() || !s.destination.is_absolute() || !s.limits.valid()
    }) {
        return Err(ServiceError::Invalid("invalid native snapshot settings"));
    }
    Ok(())
}

pub(crate) struct Lease<'a> {
    pub signals: &'a RunControl,
    pub cancel: &'a mpsc::Sender<CancelMode>,
    pub expired: &'a Arc<AtomicBool>,
    pub renew: &'a mpsc::Sender<Option<Instant>>,
    pub budget: Duration,
    pub options: RestoreOwnerOptions,
}

pub(crate) fn attempt(
    store: &mut Store,
    fence: &AttemptFence,
    settings: SnapshotSettings,
    lease: Lease<'_>,
) -> Result<()> {
    let mode = store.cancellation(fence)?;
    let local = lease.signals.relay(lease.cancel);
    if mode == Some(CancelMode::Hard) || local == Some(CancelMode::Hard) {
        store.request_cancel(&fence.session_id, CancelMode::Hard)?;
        return Ok(());
    }
    if mode != Some(CancelMode::Graceful) || lease.expired.load(Ordering::Acquire) {
        return Err(ServiceError::Coordination(
            "snapshot requires live cancelled attempt",
        ));
    }
    let timeout = Duration::from_millis(settings.limits.timeout_ms)
        .checked_add(lease.options.cleanup_timeout)
        .and_then(|duration| Instant::now().checked_add(duration))
        .ok_or(ServiceError::Invalid("snapshot deadline overflow"))?;
    let workspace = store.get(&fence.session_id)?.directory.into();
    let signals = lease.signals.clone();
    let cancel = lease.cancel.clone();
    let expired = Arc::clone(lease.expired);
    let mut task = SnapshotTask::start(settings, workspace, move || {
        expired.load(Ordering::Acquire) || signals.relay(&cancel) == Some(CancelMode::Hard)
    })?;
    let mut heartbeat = Instant::now();
    loop {
        if lease.expired.load(Ordering::Acquire) || Instant::now() >= timeout {
            return Err(ServiceError::Coordination("native snapshot owner expired"));
        }
        let local = lease.signals.relay(lease.cancel);
        if let Some(mode) = local {
            store.request_cancel(&fence.session_id, mode)?;
        }
        let hard =
            local == Some(CancelMode::Hard) || store.cancellation(fence)? == Some(CancelMode::Hard);
        if Instant::now() >= heartbeat {
            let next = Instant::now() + lease.budget;
            store.heartbeat(fence)?;
            if lease.expired.load(Ordering::Acquire) {
                return Err(ServiceError::Coordination("native snapshot lease lost"));
            }
            lease
                .renew
                .send(Some(next))
                .map_err(|_| ServiceError::Coordination("native snapshot watchdog stopped"))?;
            heartbeat = Instant::now() + lease.options.lease.poll_interval;
        }
        if let Some(result) = task.poll(hard)? {
            if lease.expired.load(Ordering::Acquire) {
                return Err(ServiceError::Coordination("native snapshot lease lost"));
            }
            cancellation_snapshot::record(store, fence, result)?;
            return Ok(());
        }
        thread::sleep(Duration::from_millis(2));
    }
}
