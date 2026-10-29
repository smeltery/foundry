use super::*;
use crate::{publication::Publication, tool_worker::WorkerControl};
use foundry_store::{AttemptFence, Store};
use std::time::Duration;

pub(super) struct Actor {
    pub options: RestoreOwnerOptions,
    pub budget: Duration,
    pub requests: mpsc::Receiver<PreparedCapture>,
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
            let version = store.get(&session)?.version;
            let (fence, admitted_session) =
                store.begin_fresh_attempt(&session, version)?.into_parts();
            // Retain the consumed one-use admission before any fallible setup.
            owned = Some((store, fence, None));
            let (store, fence, publication) = owned.as_mut().expect("fresh owner installed");
            let control = WorkerControl {
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
        let mut prepared_cleanup = false;
        let mut published = false;
        let operation = if ready && sent {
            let (store, fence, publication) = owned.as_mut().expect("fresh owner installed");
            self.serve(
                store,
                fence,
                publication.as_mut().expect("publication installed"),
                &mut prepared_cleanup,
                &mut published,
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
            if published {
                Ok(None)
            } else if !exposed || prepared_cleanup {
                (|| {
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
        prepared_cleanup: &mut bool,
        published: &mut bool,
    ) -> Result<()> {
        let mut heartbeat = Instant::now();
        loop {
            self.live()?;
            if Instant::now() >= heartbeat {
                let deadline = Instant::now() + self.budget;
                store.heartbeat(fence)?;
                self.live()?;
                if let Some(mode) = store.cancellation(fence)? {
                    let _ = self.cancel.send(mode);
                    return Err(ServiceError::Coordination("fresh attempt cancelled"));
                }
                self.renew
                    .send(Some(deadline))
                    .map_err(|_| ServiceError::Coordination("fresh watchdog stopped"))?;
                heartbeat = Instant::now() + self.options.lease.poll_interval;
            }
            publication.poll(store, fence)?;
            match self.requests.try_recv() {
                Ok(prepared) => {
                    publication.check_prepared(fence, &prepared)?;
                    *prepared_cleanup = true;
                    self.live()?;
                    let result = publication.finish(store, fence, prepared)?;
                    *published = true;
                    self.reply
                        .try_send(Ok(FreshCaptureReply::Published(Box::new(result))))
                        .map_err(|_| ServiceError::Coordination("fresh publication reply lost"))?;
                    return Ok(());
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(_) => return Err(ServiceError::Coordination("fresh caller disconnected")),
            }
            thread::sleep(Duration::from_millis(2));
        }
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
