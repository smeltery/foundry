//! Fresh-attempt entry to the same registered publication actor used after restore.
use crate::{
    Result, ServiceError,
    control::RunControl,
    lease::watch_lease,
    native_publication::{CapturePorts, PublicationConfiguration},
    restoration_owner::{RestoreOwnerOptions, RestoreOwnerReport},
};
use foundry_contracts::CancelMode;
use foundry_runtime::coordinated::{CaptureActionRequired, PreparedCapture};
use foundry_store::FinalizedCheckpoint;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Instant,
};
mod worker;

pub enum FreshCaptureReply {
    Ready(Box<CapturePorts>),
    Published(Box<FinalizedCheckpoint>),
    ActionRequired(Box<foundry_contracts::Session>),
}

enum Job {
    FinishCapture(Box<PreparedCapture>),
    FinishActionRequired(CaptureActionRequired),
}

pub struct FreshCaptureOwner {
    jobs: mpsc::SyncSender<Job>,
    replies: mpsc::Receiver<Result<FreshCaptureReply>>,
    pending: bool,
    submitted: bool,
    cancellation: Option<mpsc::Receiver<CancelMode>>,
    cancel: mpsc::Sender<CancelMode>,
    requested: Arc<AtomicU8>,
    signals: RunControl,
    stopping: Arc<AtomicBool>,
    expired: Arc<AtomicBool>,
    renew: mpsc::Sender<Option<Instant>>,
    actor: Option<JoinHandle<RestoreOwnerReport>>,
    watchdog: Option<JoinHandle<()>>,
}
impl FreshCaptureOwner {
    pub fn start(
        path: PathBuf,
        session_id: String,
        configuration: PublicationConfiguration,
        options: RestoreOwnerOptions,
    ) -> Result<Self> {
        Self::start_inner(path, session_id, configuration, options, None)
    }

    /// The runtime must use the matching policy in required authorization mode.
    pub fn start_with_permissions(
        path: PathBuf,
        session_id: String,
        configuration: PublicationConfiguration,
        options: RestoreOwnerOptions,
        permissions: crate::permission::ToolPermissions,
    ) -> Result<Self> {
        permissions.check_configuration(&configuration.profile.configuration_digest)?;
        Self::start_inner(
            path,
            session_id,
            configuration,
            options,
            Some(Arc::new(permissions)),
        )
    }

    fn start_inner(
        path: PathBuf,
        session_id: String,
        configuration: PublicationConfiguration,
        options: RestoreOwnerOptions,
        permissions: Option<Arc<crate::permission::ToolPermissions>>,
    ) -> Result<Self> {
        // Fresh capture is deliberately a first-attempt entry, never restore fallback.
        if path.as_os_str().is_empty()
            || path == std::path::Path::new(":memory:")
            || options.lease.poll_interval.is_zero()
            || options.lease.poll_interval >= options.lease.lease_duration / 3
        {
            return Err(ServiceError::Invalid("invalid fresh capture options"));
        }
        let budget = options
            .lease
            .lease_duration
            .checked_mul(2)
            .map(|d| d / 3)
            .filter(|d| !d.is_zero())
            .ok_or(ServiceError::Invalid("invalid fresh lease duration"))?;
        let deadline = Instant::now()
            .checked_add(budget)
            .ok_or(ServiceError::Invalid("fresh deadline overflow"))?;
        let (jobs, requests) = mpsc::sync_channel(1);
        let (reply, replies) = mpsc::sync_channel(1);
        let (cancel, cancellation) = mpsc::channel();
        let (renew, renewed) = mpsc::channel();
        let requested = Arc::new(AtomicU8::new(0));
        let signals = RunControl::new(Arc::clone(&requested));
        signals.permissions.initialize(deadline);
        let stopping = Arc::new(AtomicBool::new(false));
        let expired = Arc::new(AtomicBool::new(false));
        let (watch_expired, watch_cancel, watch_signals) =
            (Arc::clone(&expired), cancel.clone(), signals.clone());
        let watchdog = thread::Builder::new()
            .name("foundry-fresh-deadline".into())
            .spawn(move || {
                watch_lease(
                    deadline,
                    renewed,
                    watch_expired,
                    watch_cancel,
                    watch_signals,
                )
            })
            .map_err(|_| ServiceError::Coordination("fresh watchdog unavailable"))?;
        let mut owner = Self {
            jobs,
            replies,
            pending: true,
            submitted: false,
            cancellation: Some(cancellation),
            cancel: cancel.clone(),
            requested,
            signals: signals.clone(),
            stopping: Arc::clone(&stopping),
            expired: Arc::clone(&expired),
            renew: renew.clone(),
            actor: None,
            watchdog: Some(watchdog),
        };
        let actor = worker::Actor {
            permissions,
            options,
            budget,
            requests,
            reply,
            cancel,
            signals,
            stopping,
            expired,
            renew,
        };
        owner.actor = Some(
            thread::Builder::new()
                .name("foundry-fresh-owner".into())
                .spawn(move || actor.run(path, session_id, configuration))
                .map_err(|_| ServiceError::Coordination("fresh owner unavailable"))?,
        );
        Ok(owner)
    }
    pub fn take_cancellation(&mut self) -> Result<mpsc::Receiver<CancelMode>> {
        self.cancellation.take().ok_or(ServiceError::Coordination(
            "fresh cancellation already taken",
        ))
    }
    pub fn try_recv(&mut self) -> Result<Option<FreshCaptureReply>> {
        match self.replies.try_recv() {
            Ok(reply) => {
                self.pending = false;
                reply.map(Some)
            }
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(_) => Err(ServiceError::Coordination("fresh owner stopped")),
        }
    }
    pub fn finish_capture(&mut self, prepared: PreparedCapture) -> Result<()> {
        self.submit(Job::FinishCapture(Box::new(prepared)))
    }
    pub fn finish_action_required(&mut self, stop: CaptureActionRequired) -> Result<()> {
        self.submit(Job::FinishActionRequired(stop))
    }
    fn submit(&mut self, job: Job) -> Result<()> {
        if self.pending
            || self.submitted
            || self.expired.load(Ordering::Acquire)
            || self.stopping.load(Ordering::Acquire)
        {
            return Err(ServiceError::Coordination("fresh finalization unavailable"));
        }
        self.jobs
            .try_send(job)
            .map_err(|_| ServiceError::Coordination("fresh finalization queue unavailable"))?;
        self.pending = true;
        self.submitted = true;
        Ok(())
    }
    pub fn cancel(&self, mode: CancelMode) {
        self.requested.fetch_max(
            if mode == CancelMode::Hard { 2 } else { 1 },
            Ordering::AcqRel,
        );
        self.signals.relay(&self.cancel);
    }
    pub fn shutdown(mut self) -> RestoreOwnerReport {
        self.stop();
        self.join()
    }
    fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        self.signals.stop_admission();
        let _ = self.cancel.send(CancelMode::Hard);
    }
    fn join(&mut self) -> RestoreOwnerReport {
        let mut report = match self.actor.take().map(JoinHandle::join) {
            Some(Ok(report)) => report,
            Some(Err(_)) => RestoreOwnerReport {
                expired: self.expired.load(Ordering::Acquire),
                watchdog_panicked: false,
                operation: Err(ServiceError::Coordination("fresh owner panicked")),
                release: Err(ServiceError::Coordination("fresh cleanup unverified")),
            },
            None => RestoreOwnerReport {
                expired: self.expired.load(Ordering::Acquire),
                watchdog_panicked: false,
                operation: Ok(()),
                release: Ok(None),
            },
        };
        let _ = self.renew.send(None);
        report.watchdog_panicked = self.watchdog.take().is_some_and(|w| w.join().is_err());
        report.expired |= self.expired.load(Ordering::Acquire);
        report
    }
}
impl Drop for FreshCaptureOwner {
    fn drop(&mut self) {
        self.stop();
        let _ = self.join();
    }
}
