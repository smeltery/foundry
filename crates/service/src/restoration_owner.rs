//! Store-owned loading reservations. No native confirmation or execution admission.
mod binding;
mod continuation;
pub(crate) mod validation;
pub use continuation::ContinueSelection;
use foundry_runtime::coordinated::loading::continuation::ContinuationOwner;
mod capture;
mod worker;
use crate::native_validation::{NativeExpectations, NativeValidatorRegistration};
use foundry_contracts::context_observation::{ObservationBinding, ObservationFrame};

use crate::{Result, ServiceError, ServiceOptions, control::RunControl, lease::watch_lease};
use foundry_contracts::{
    CancelMode,
    context::{ContextDigest, ContextId, ContextReceipt},
    context_v2::{LoadingScope, RestoreDescriptor},
};
use foundry_runtime::coordinated::loading::{CleanupObserver, LoadedProcess};
use foundry_store::{BeginRestoreInput, CheckpointRef, RestoreBaselineSnapshot, RestoreRef};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub struct RestoreOwnerOptions {
    pub lease: ServiceOptions,
    pub cleanup_timeout: Duration,
}

pub enum RestoreReply {
    Reserved {
        scope: LoadingScope,
        checkpoint: Box<CheckpointRef>,
    },
    Baseline(Arc<RestoreBaselineSnapshot>),
    PlanBound(Box<RestoreDescriptor>),
    CleanupRegistered,
    NativePrepared(ObservationBinding),
    NativeStarted,
    NativePending,
    NativeValidated(ContextDigest),
    Continuation(ContinuationOwner),
    CaptureReady(Box<crate::native_publication::CapturePorts>),
    Published(Box<foundry_store::FinalizedCheckpoint>),
    ActionRequired(Box<foundry_contracts::Session>),
}

/// A correlated result, not restoration or execution authority.
pub struct OwnerReply {
    pub job_id: ContextReceipt,
    pub result: Result<RestoreReply>,
}

#[derive(Debug)]
pub struct RestoreOwnerReport {
    pub expired: bool,
    pub watchdog_panicked: bool,
    pub operation: Result<()>,
    pub release: Result<Option<RestoreRef>>,
}

enum JobKind {
    Baseline,
    PrepareNative(NativeValidatorRegistration, NativeExpectations),
    ValidateNative {
        frame: ObservationFrame,
        instance: ContextReceipt,
        scope: LoadingScope,
        begin: ContextDigest,
    },
    NativeStatus,
    OpenContinuation(ContinueSelection),
    OpenCapture(
        Box<crate::native_publication::PublicationConfiguration>,
        Option<Arc<crate::permission::ToolPermissions>>,
    ),
    FinishCapture(Box<foundry_runtime::coordinated::PreparedCapture>),
    FinishActionRequired(Box<foundry_runtime::coordinated::CaptureActionRequired>),
    BindPlan(Box<RestoreDescriptor>),
    RegisterCleanup {
        scope: LoadingScope,
        binding: ContextDigest,
        observer: CleanupObserver,
    },
}
struct Job {
    id: ContextReceipt,
    kind: JobKind,
}

/// The runtime must keep polling its own process independently of this actor.
pub struct RestorationOwner {
    jobs: SyncSender<Job>,
    replies: Receiver<OwnerReply>,
    pending: Option<ContextReceipt>,
    cancellation: Option<Receiver<CancelMode>>,
    cancel_tx: mpsc::Sender<CancelMode>,
    requested: Arc<AtomicU8>,
    signals: RunControl,
    stopping: Arc<AtomicBool>,
    expired: Arc<AtomicBool>,
    renew: mpsc::Sender<Option<Instant>>,
    actor: Option<JoinHandle<RestoreOwnerReport>>,
    watchdog: Option<JoinHandle<()>>,
}

fn id() -> Result<ContextReceipt> {
    ContextReceipt::new(uuid::Uuid::new_v4().to_string())
        .map_err(|_| ServiceError::Coordination("cannot allocate restore job identity"))
}

impl RestorationOwner {
    /// All database work, including opening and reserving, runs on the actor.
    pub fn start(
        path: PathBuf,
        session_id: String,
        input: BeginRestoreInput,
        options: RestoreOwnerOptions,
    ) -> Result<Self> {
        if path.as_os_str().is_empty() || path == std::path::Path::new(":memory:") {
            return Err(ServiceError::Invalid(
                "persistent restoration database required",
            ));
        }
        let budget = options
            .lease
            .lease_duration
            .checked_mul(2)
            .map(|value| value / 3)
            .filter(|value| !value.is_zero())
            .ok_or(ServiceError::Invalid("invalid restoration lease duration"))?;
        let now = Instant::now();
        let deadline = now
            .checked_add(budget)
            .ok_or(ServiceError::Invalid("restoration deadline overflow"))?;
        if options.lease.poll_interval.is_zero()
            || options.lease.poll_interval >= options.lease.lease_duration / 3
            || options.cleanup_timeout.is_zero()
            || now.checked_add(options.cleanup_timeout).is_none()
        {
            return Err(ServiceError::Invalid(
                "invalid restoration polling or cleanup duration",
            ));
        }
        let initial = id()?;
        let load_id = id()?;
        let (jobs, received) = mpsc::sync_channel(1);
        let (reply, replies) = mpsc::sync_channel(1);
        let (cancel_tx, cancellation) = mpsc::channel();
        let (renew, renewed) = mpsc::channel();
        let requested = Arc::new(AtomicU8::new(0));
        let signals = RunControl::new(Arc::clone(&requested));
        signals.permissions.initialize(deadline);
        let expired = Arc::new(AtomicBool::new(false));
        let stopping = Arc::new(AtomicBool::new(false));
        let watch_signals = signals.clone();
        let watch_expired = Arc::clone(&expired);
        let watch_cancel = cancel_tx.clone();
        let watchdog = thread::Builder::new()
            .name("foundry-restore-deadline".into())
            .spawn(move || {
                watch_lease(
                    deadline,
                    renewed,
                    watch_expired,
                    watch_cancel,
                    watch_signals,
                )
            })
            .map_err(|_| ServiceError::Coordination("cannot start restore watchdog"))?;
        let mut owner = Self {
            jobs,
            replies,
            pending: Some(initial.clone()),
            cancellation: Some(cancellation),
            cancel_tx: cancel_tx.clone(),
            requested,
            signals: signals.clone(),
            stopping: Arc::clone(&stopping),
            expired: Arc::clone(&expired),
            renew: renew.clone(),
            actor: None,
            watchdog: Some(watchdog),
        };
        let actor = worker::Actor::new(
            options,
            budget,
            worker::Channels {
                jobs: received,
                replies: reply,
                cancel: cancel_tx,
                signals,
                stopping,
                expired,
                renew,
            },
        );
        owner.actor = Some(
            thread::Builder::new()
                .name("foundry-restore-owner".into())
                .spawn(move || actor.run(path, session_id, input, initial, load_id))
                .map_err(|_| ServiceError::Coordination("cannot start restore owner"))?,
        );
        Ok(owner)
    }

    pub fn pending_job(&self) -> Option<&ContextReceipt> {
        self.pending.as_ref()
    }

    pub fn take_cancellation(&mut self) -> Result<Receiver<CancelMode>> {
        self.cancellation.take().ok_or(ServiceError::Coordination(
            "restore cancellation receiver already taken",
        ))
    }

    pub fn try_recv(&mut self) -> Result<Option<OwnerReply>> {
        match self.replies.try_recv() {
            Ok(reply) => {
                if self.pending.as_ref() != Some(&reply.job_id) {
                    self.request_shutdown();
                    return Err(ServiceError::Coordination("unexpected restore job reply"));
                }
                self.pending = None;
                Ok(Some(reply))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err(ServiceError::Coordination("restore owner stopped"))
            }
        }
    }

    pub fn request_baseline(&mut self) -> Result<ContextReceipt> {
        self.submit(JobKind::Baseline)
    }

    /// Pin an S1-correlated descriptor before loading. Transport/profile fields remain
    /// trusted caller configuration, not native authority or a type-level proof.
    pub fn bind_plan(&mut self, descriptor: RestoreDescriptor) -> Result<ContextReceipt> {
        self.submit(JobKind::BindPlan(Box::new(descriptor)))
    }

    /// Capture cleanup evidence from the actual retained runtime owner, never a bool.
    pub fn register_loaded(&mut self, loaded: &LoadedProcess) -> Result<ContextReceipt> {
        self.submit(JobKind::RegisterCleanup {
            scope: loaded.scope().clone(),
            binding: loaded.begin_binding(),
            observer: loaded.cleanup_observer(),
        })
    }

    pub fn prepare_native(
        &mut self,
        registration: NativeValidatorRegistration,
        expected: NativeExpectations,
    ) -> Result<ContextReceipt> {
        self.submit(JobKind::PrepareNative(registration, expected))
    }
    /// The trusted supervisor supplies the exact frame returned by this held owner.
    pub fn validate_native(
        &mut self,
        loaded: &LoadedProcess,
        frame: ObservationFrame,
    ) -> Result<ContextReceipt> {
        let instance = loaded
            .observed_instance()
            .cloned()
            .ok_or(ServiceError::Coordination("runtime observation incomplete"))?;
        self.submit(JobKind::ValidateNative {
            frame,
            instance,
            scope: loaded.scope().clone(),
            begin: loaded.begin_binding(),
        })
    }
    pub fn native_status(&mut self) -> Result<ContextReceipt> {
        self.submit(JobKind::NativeStatus)
    }

    pub fn open_continuation(&mut self, selection: ContinueSelection) -> Result<ContextReceipt> {
        self.submit(JobKind::OpenContinuation(selection))
    }

    fn submit(&mut self, kind: JobKind) -> Result<ContextReceipt> {
        if self.pending.is_some()
            || self.stopping.load(Ordering::Acquire)
            || self.expired.load(Ordering::Acquire)
        {
            return Err(ServiceError::Coordination("restore owner busy or stopped"));
        }
        let id = id()?;
        self.jobs
            .try_send(Job {
                id: id.clone(),
                kind,
            })
            .map_err(|_| ServiceError::Coordination("restore job queue unavailable"))?;
        self.pending = Some(id.clone());
        Ok(id)
    }

    pub fn cancel(&self, mode: CancelMode) {
        self.requested.fetch_max(
            if mode == CancelMode::Hard { 2 } else { 1 },
            Ordering::AcqRel,
        );
        self.signals.relay(&self.cancel_tx);
    }

    pub fn expired(&self) -> bool {
        self.expired.load(Ordering::Acquire)
    }

    fn request_shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        self.signals.stop_admission();
        let _ = self.cancel_tx.send(CancelMode::Hard);
    }

    /// Runtime cleanup/polling must remain independent while actor SQL drains.
    pub fn shutdown(mut self) -> RestoreOwnerReport {
        self.request_shutdown();
        self.join()
    }

    fn join(&mut self) -> RestoreOwnerReport {
        let mut report = match self.actor.take().map(JoinHandle::join) {
            Some(Ok(report)) => report,
            Some(Err(_)) => RestoreOwnerReport {
                expired: self.expired(),
                watchdog_panicked: false,
                operation: Err(ServiceError::Coordination("restore owner panicked")),
                release: Err(ServiceError::Coordination("restore release unverified")),
            },
            None => RestoreOwnerReport {
                expired: self.expired(),
                watchdog_panicked: false,
                operation: Ok(()),
                release: Ok(None),
            },
        };
        let _ = self.renew.send(None);
        report.watchdog_panicked = self.watchdog.take().is_some_and(|h| h.join().is_err());
        report.expired |= self.expired();
        report
    }
}

impl Drop for RestorationOwner {
    fn drop(&mut self) {
        self.request_shutdown();
        // Fallback only; explicit shutdown reports every join/release outcome.
        let _ = self.join();
    }
}
