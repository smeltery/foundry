use super::*;
mod jobs;
mod release;
use super::validation::NativeState;
use foundry_runtime::coordinated::loading::CleanupStatus;
use foundry_store::{BeginRestoreResult, RestoreFence, Store};

pub(super) struct Channels {
    pub jobs: Receiver<Job>,
    pub replies: SyncSender<OwnerReply>,
    pub cancel: mpsc::Sender<CancelMode>,
    pub signals: RunControl,
    pub stopping: Arc<AtomicBool>,
    pub expired: Arc<AtomicBool>,
    pub renew: mpsc::Sender<Option<Instant>>,
}
pub(super) struct Actor {
    snapshot_settings: Option<crate::cancellation_snapshot::SnapshotSettings>,
    options: RestoreOwnerOptions,
    budget: Duration,
    io: Channels,
}
struct Reservation {
    session: String,
    fence: RestoreFence,
    scope: LoadingScope,
    snapshot: Option<Arc<RestoreBaselineSnapshot>>,
    exposed: bool,
    binding: Option<ContextDigest>,
    cleanup: Option<CleanupObserver>,
    native: Option<NativeState>,
    continuation: Option<super::continuation::Link>,
    publication: Option<crate::publication::Publication>,
    attempt_finalized: bool,
    cancellation: Option<RestoredCancellation>,
}

impl Actor {
    pub fn new(options: RestoreOwnerOptions, budget: Duration, io: Channels) -> Self {
        Self {
            snapshot_settings: None,
            options,
            budget,
            io,
        }
    }

    pub fn with_snapshot(
        mut self,
        settings: Option<crate::cancellation_snapshot::SnapshotSettings>,
    ) -> Self {
        self.snapshot_settings = settings;
        self
    }
    pub fn run(
        self,
        path: PathBuf,
        session: String,
        input: BeginRestoreInput,
        initial: ContextReceipt,
        load_id: ContextReceipt,
    ) -> RestoreOwnerReport {
        let mut reservation = None;
        let mut store = match Store::open_with_lease(path, self.options.lease.lease_duration) {
            Ok(store) => store,
            Err(error) => {
                let _ = self.reply(initial, Err(error.into()));
                return self.report(
                    Err(ServiceError::Coordination(
                        "restoration database unavailable",
                    )),
                    Ok(None),
                );
            }
        };
        let acquired = (|| {
            self.live(&mut store, &session)?;
            let BeginRestoreResult::Started { fence, checkpoint } =
                store.begin_restore(&session, input)?
            else {
                return Err(ServiceError::Coordination("restore retry is metadata only"));
            };
            let scope = LoadingScope {
                restore_id: ContextId::new(fence.restore_id())
                    .map_err(|_| ServiceError::Coordination("invalid stored restore identity"))?,
                load_id,
            };
            reservation = Some(Reservation {
                session: session.clone(),
                fence,
                scope: scope.clone(),
                snapshot: None,
                exposed: false,
                binding: None,
                cleanup: None,
                native: None,
                continuation: None,
                publication: None,
                attempt_finalized: false,
                cancellation: None,
            });
            let selected = reservation.as_mut().expect("reservation installed");
            selected.snapshot = Some(Arc::new(store.restore_baseline_snapshot(&selected.fence)?));
            self.live(&mut store, &session)?;
            Ok(RestoreReply::Reserved { scope, checkpoint })
        })();
        let initialized = acquired.is_ok();
        let sent = self.reply(initial, acquired);
        let operation = if !initialized {
            Err(ServiceError::Coordination("restoration reservation failed"))
        } else if let Err(error) = sent {
            Err(error)
        } else {
            self.serve(
                &mut store,
                reservation.as_mut().expect("reservation installed"),
            )
        };
        self.io.signals.stop_admission();
        let _ = self.io.cancel.send(CancelMode::Hard);
        if let Some(native) = reservation.as_ref().and_then(|r| r.native.as_ref()) {
            native.cancel();
        }
        if let Some(publication) = reservation.as_ref().and_then(|r| r.publication.as_ref()) {
            publication.cancel();
        }
        let release = match reservation.as_mut() {
            Some(reservation) => self.release(&mut store, reservation, operation.is_err()),
            None => Ok(None),
        };
        let mut report = self.report(operation, release);
        if !report.expired
            && report.release.is_ok()
            && super::settlement::cancelled_operation(&report.operation)
        {
            report.cancellation =
                reservation.and_then(|mut reservation| reservation.cancellation.take());
        }
        report
    }

    fn report(
        &self,
        operation: Result<()>,
        release: Result<Option<RestoreRef>>,
    ) -> RestoreOwnerReport {
        self.io.signals.stop_admission();
        let _ = self.io.cancel.send(CancelMode::Hard);
        RestoreOwnerReport {
            cancellation: None,
            expired: self.io.expired.load(Ordering::Acquire),
            watchdog_panicked: false,
            operation,
            release,
        }
    }

    fn live(&self, store: &mut Store, session: &str) -> Result<()> {
        self.live_authority(store, session)?;
        if self.io.stopping.load(Ordering::Acquire) {
            return Err(ServiceError::Coordination("restoration shutdown requested"));
        }
        Ok(())
    }

    fn live_authority(&self, store: &mut Store, session: &str) -> Result<()> {
        // Relay before SQL; the watchdog also observes this same sticky control.
        if let Some(mode) = self.io.signals.relay(&self.io.cancel) {
            store.request_cancel(session, mode)?;
            return Err(ServiceError::Coordination("restoration cancelled"));
        }
        if self.io.expired.load(Ordering::Acquire) {
            return Err(ServiceError::Coordination(
                "restoration lease deadline elapsed",
            ));
        }
        Ok(())
    }

    fn heartbeat(&self, store: &mut Store, reservation: &Reservation) -> Result<()> {
        let deadline =
            Instant::now()
                .checked_add(self.budget)
                .ok_or(ServiceError::Coordination(
                    "restore heartbeat deadline overflow",
                ))?;
        if let Some(grant) = reservation
            .continuation
            .as_ref()
            .and_then(|link| link.grant.as_ref())
        {
            store.heartbeat(&grant.fence)?;
        } else {
            store.heartbeat_restore(&reservation.fence)?;
        }
        if self.io.expired.load(Ordering::Acquire) {
            return Err(ServiceError::Coordination("late restore heartbeat"));
        }
        self.io
            .renew
            .send(Some(deadline))
            .map_err(|_| ServiceError::Coordination("restore watchdog stopped"))?;
        Ok(())
    }

    fn serve(&self, store: &mut Store, reservation: &mut Reservation) -> Result<()> {
        let result = self.serve_active(store, reservation);
        self.finish_serving(store, &reservation.session, result)
    }

    fn finish_serving(&self, store: &mut Store, session: &str, result: Result<()>) -> Result<()> {
        match result {
            // Shutdown can arrive at any live check after the loop's initial check.
            // Only this local control outcome is normal; authority failures survive.
            Err(ServiceError::Coordination("restoration shutdown requested")) => {
                self.live_authority(store, session)
            }
            other => other,
        }
    }

    fn serve_active(&self, store: &mut Store, reservation: &mut Reservation) -> Result<()> {
        let mut next_heartbeat = Instant::now();
        loop {
            if self.io.stopping.load(Ordering::Acquire) {
                return self.live_authority(store, &reservation.session);
            }
            if reservation.attempt_finalized {
                return Ok(());
            }
            self.live(store, &reservation.session)?;
            if Instant::now() >= next_heartbeat {
                self.heartbeat(store, reservation)?;
                self.live(store, &reservation.session)?;
                let cancel = if let Some(grant) = reservation
                    .continuation
                    .as_ref()
                    .and_then(|link| link.grant.as_ref())
                {
                    store.cancellation(&grant.fence)?
                } else {
                    store.restore_cancellation(&reservation.fence)?
                };
                if let Some(mode) = cancel {
                    self.io.signals.stop_admission();
                    let _ = self.io.cancel.send(mode);
                    return Err(ServiceError::Coordination(
                        "stored restoration cancellation",
                    ));
                }
                next_heartbeat = Instant::now() + self.options.lease.poll_interval;
            }
            if let Some(native) = reservation.native.as_mut()
                && let Some(validated) = native.poll()?
            {
                self.live(store, &reservation.session)?;
                let fresh = store.restore_baseline_snapshot(&reservation.fence)?;
                let selected = reservation.snapshot.as_ref().expect("selected snapshot");
                if fresh.checkpoint().reference() != selected.checkpoint().reference()
                    || fresh.witness().digest() != selected.witness().digest()
                    || fresh.facts().cut != selected.facts().cut
                {
                    return Err(ServiceError::Coordination("native selection changed"));
                }
                self.live(store, &reservation.session)?;
                if reservation.cleanup.as_ref().map(CleanupObserver::status)
                    != Some(CleanupStatus::Pending)
                {
                    return Err(ServiceError::Coordination(
                        "native runtime cleanup already started",
                    ));
                }
                store.confirm_restored(
                    &reservation.fence,
                    fresh.checkpoint().reference().configuration_digest,
                )?;
                self.live(store, &reservation.session)?;
                native.validated = Some(validated);
            }
            if let Some(link) = reservation.continuation.as_mut() {
                let validated = reservation
                    .native
                    .as_ref()
                    .and_then(|native| native.validated.as_ref())
                    .ok_or(ServiceError::Coordination("native validation absent"))?;
                link.poll(
                    store,
                    &reservation.fence,
                    reservation.snapshot.as_ref().expect("selected snapshot"),
                    validated,
                    || self.continuation_authority(reservation.cleanup.as_ref()),
                )?;
            }
            if let Some(publication) = reservation.publication.as_mut() {
                let grant = reservation
                    .continuation
                    .as_ref()
                    .and_then(|link| link.grant.as_ref())
                    .ok_or(ServiceError::Coordination("capture attempt absent"))?;
                publication.poll(store, &grant.fence)?;
            }
            let wait = next_heartbeat
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(5));
            match self.io.jobs.recv_timeout(wait) {
                Ok(job) => {
                    self.live(store, &reservation.session)?;
                    let result = self.job(store, reservation, job.kind);
                    let success = result.is_ok();
                    self.reply(job.id, result)?;
                    if !success {
                        return Err(ServiceError::Coordination("restore owner job failed"));
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ServiceError::Coordination("restore caller disconnected"));
                }
            }
        }
    }

    fn continuation_authority(&self, cleanup: Option<&CleanupObserver>) -> Result<()> {
        if self.io.expired.load(Ordering::Acquire) {
            return Err(ServiceError::Coordination(
                "restoration lease deadline elapsed",
            ));
        }
        if self.io.signals.relay(&self.io.cancel).is_some() {
            return Err(ServiceError::Coordination("restoration cancelled"));
        }
        if self.io.stopping.load(Ordering::Acquire) {
            return Err(ServiceError::Coordination("continuation owner stopped"));
        }
        if cleanup.map(CleanupObserver::status) != Some(CleanupStatus::Pending) {
            return Err(ServiceError::Coordination(
                "native runtime cleanup already started",
            ));
        }
        Ok(())
    }

    fn reply(&self, job_id: ContextReceipt, result: Result<RestoreReply>) -> Result<()> {
        self.io
            .replies
            .try_send(OwnerReply { job_id, result })
            .map_err(|_| ServiceError::Coordination("restore reply unavailable"))
    }
}

#[cfg(test)]
mod shutdown_tests;
