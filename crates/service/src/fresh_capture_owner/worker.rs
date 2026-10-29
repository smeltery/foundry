use super::*;
mod cancellation;
use crate::{publication::Publication, tool_worker::WorkerControl};
use foundry_store::{AttemptFence, Store};
use std::time::Duration;

pub(super) struct Actor {
    pub snapshot: Option<crate::cancellation_snapshot::SnapshotSettings>,
    pub permissions: Option<Arc<crate::permission::ToolPermissions>>,
    pub options: RestoreOwnerOptions,
    pub budget: Duration,
    pub requests: mpsc::Receiver<Job>,
    pub reply: mpsc::SyncSender<Result<FreshCaptureReply>>,
    pub cancel: mpsc::Sender<CancelMode>,
    pub signals: RunControl,
    pub stopping: Arc<AtomicBool>,
    pub expired: Arc<AtomicBool>,
    pub renew: mpsc::Sender<Option<Instant>>,
}
impl Actor {
    pub fn run(
        self,
        path: PathBuf,
        session: String,
        configuration: PublicationConfiguration,
    ) -> RestoreOwnerReport {
        let mut owned: Option<(Store, AttemptFence, Option<Publication>)> = None;
        let mut exposed = false;
        let initial = (|| {
            let mut store = Store::open_with_lease(path, self.options.lease.lease_duration)?;
            self.live()?;
            if let Some(permissions) = &self.permissions {
                permissions.check_configuration(&configuration.profile.configuration_digest)?;
            }
            let version = store.get(&session)?.version;
            let (fence, admitted_session) =
                store.begin_fresh_attempt(&session, version)?.into_parts();
            // Retain the consumed one-use admission before any fallible setup.
            owned = Some((store, fence, None));
            let (store, fence, publication) = owned.as_mut().expect("fresh owner installed");
            let control = WorkerControl {
                permissions: self.permissions.clone(),
                signals: self.signals.clone(),
                activate_on_ready: false,
                expired: Arc::clone(&self.expired),
                cancellation: self.cancel.clone(),
            };
            let (opened, mut ports) =
                Publication::new(store, fence, configuration, control, None, None)?;
            *publication = Some(opened);
            ports.session = admitted_session;
            self.live()?;
            Ok(FreshCaptureReply::Ready(Box::new(ports)))
        })();
        let ready = initial.is_ok();
        let sent = self.reply.try_send(initial).is_ok();
        if ready && sent {
            exposed = true;
        }
        let mut runtime_cleanup_verified = false;
        let mut attempt_finalized = false;
        let operation = if ready && sent {
            let (store, fence, publication) = owned.as_mut().expect("fresh owner installed");
            self.serve(
                store,
                fence,
                publication.as_mut().expect("publication installed"),
                &mut runtime_cleanup_verified,
                &mut attempt_finalized,
            )
        } else {
            Err(ServiceError::Coordination(
                "fresh capture initialization failed",
            ))
        };
        self.signals.stop_admission();
        let _ = self.cancel.send(CancelMode::Hard);
        let release = if let Some((store, fence, publication)) = owned.as_mut() {
            if let Some(publication) = publication {
                publication.cancel();
            }
            if attempt_finalized {
                Ok(None)
            } else if !exposed || runtime_cleanup_verified {
                (|| {
                    if self.expired.load(Ordering::Acquire) {
                        return Err(ServiceError::Coordination(
                            "fresh lease expired; attempt retained",
                        ));
                    }
                    if let Some(publication) = publication
                        && !publication.cancelled_workers_complete()?
                    {
                        return Err(ServiceError::Coordination(
                            "fresh publication cleanup incomplete",
                        ));
                    }
                    if let Some(mode) = self.signals.relay(&self.cancel) {
                        store.request_cancel(&session, mode)?;
                    }
                    let lifecycle = if store.cancellation(fence)?.is_some() {
                        foundry_contracts::Lifecycle::Suspended
                    } else {
                        foundry_contracts::Lifecycle::Error
                    };
                    store.finish_attempt(fence, lifecycle, None, None)?;
                    Ok(None)
                })()
            } else {
                Err(ServiceError::Coordination(
                    "fresh runtime cleanup unverified; attempt retained",
                ))
            }
        } else {
            Ok(None)
        };
        RestoreOwnerReport {
            cancellation: None,
            expired: self.expired.load(Ordering::Acquire),
            watchdog_panicked: false,
            operation,
            release,
        }
    }
    fn serve(
        &self,
        store: &mut Store,
        fence: &AttemptFence,
        publication: &mut Publication,
        runtime_cleanup_verified: &mut bool,
        attempt_finalized: &mut bool,
    ) -> Result<()> {
        let mut heartbeat = Instant::now();
        loop {
            if self.cancellation_mode(store, fence)?.is_some() {
                return self.drain_cancelled(
                    store,
                    fence,
                    publication,
                    runtime_cleanup_verified,
                    attempt_finalized,
                );
            }
            self.owner_live()?;
            if Instant::now() >= heartbeat {
                let deadline = Instant::now() + self.budget;
                store.heartbeat(fence)?;
                self.owner_live()?;
                if let Some(mode) = store.cancellation(fence)? {
                    let _ = self.cancel.send(mode);
                    return self.drain_cancelled(
                        store,
                        fence,
                        publication,
                        runtime_cleanup_verified,
                        attempt_finalized,
                    );
                }
                self.renew
                    .send(Some(deadline))
                    .map_err(|_| ServiceError::Coordination("fresh watchdog stopped"))?;
                heartbeat = Instant::now() + self.options.lease.poll_interval;
            }
            if let Err(error) = publication.poll(store, fence) {
                if self.cancellation_mode(store, fence)?.is_some() {
                    return self.drain_cancelled(
                        store,
                        fence,
                        publication,
                        runtime_cleanup_verified,
                        attempt_finalized,
                    );
                }
                return Err(error);
            }
            match self.requests.try_recv() {
                Ok(job) => {
                    let reply = match job {
                        Job::Cancelled(stop) => {
                            publication.check_cancelled(fence, &stop)?;
                            *runtime_cleanup_verified = true;
                            store.request_cancel(&fence.session_id, stop.mode())?;
                            let _cleanup = stop.into_cleanup();
                            return self.drain_cancelled(
                                store,
                                fence,
                                publication,
                                runtime_cleanup_verified,
                                attempt_finalized,
                            );
                        }
                        Job::Capture(prepared) => {
                            publication.check_prepared(fence, &prepared)?;
                            *runtime_cleanup_verified = true;
                            self.live()?;
                            FreshCaptureReply::Published(Box::new(
                                publication.finish(store, fence, *prepared)?,
                            ))
                        }
                        Job::ActionRequired(stop) => {
                            publication.check_action_required(fence, &stop)?;
                            *runtime_cleanup_verified = true;
                            self.live()?;
                            FreshCaptureReply::ActionRequired(Box::new(
                                publication.finish_action_required(store, fence, stop)?,
                            ))
                        }
                    };
                    *attempt_finalized = true;
                    self.reply
                        .try_send(Ok(reply))
                        .map_err(|_| ServiceError::Coordination("fresh publication reply lost"))?;
                    return Ok(());
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(_) => return Err(ServiceError::Coordination("fresh caller disconnected")),
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn owner_live(&self) -> Result<()> {
        if self.expired.load(Ordering::Acquire) || self.stopping.load(Ordering::Acquire) {
            return Err(ServiceError::Coordination(
                "fresh capture stopped or expired",
            ));
        }
        Ok(())
    }
    fn live(&self) -> Result<()> {
        if self.expired.load(Ordering::Acquire)
            || self.stopping.load(Ordering::Acquire)
            || self.signals.relay(&self.cancel).is_some()
        {
            return Err(ServiceError::Coordination(
                "fresh capture cancelled or expired",
            ));
        }
        Ok(())
    }
}
