//! Cancelled execution keeps its owner alive only for bounded cleanup settlement.
use super::*;
use foundry_contracts::Lifecycle;
impl Actor {
    pub(super) fn cancellation_mode(
        &self,
        store: &Store,
        fence: &AttemptFence,
    ) -> Result<Option<CancelMode>> {
        let local = self.signals.relay(&self.cancel);
        let durable = store.cancellation(fence)?;
        Ok(match (local, durable) {
            (Some(CancelMode::Hard), _) | (_, Some(CancelMode::Hard)) => Some(CancelMode::Hard),
            (Some(mode), _) | (_, Some(mode)) => Some(mode),
            _ => None,
        })
    }
    pub(super) fn drain_cancelled(
        &self,
        store: &mut Store,
        fence: &AttemptFence,
        publication: &mut Publication,
        cleanup: &mut bool,
        finalized: &mut bool,
    ) -> Result<()> {
        self.signals.stop_admission();
        publication.cancel();
        let deadline = Instant::now()
            .checked_add(self.options.cleanup_timeout)
            .ok_or(ServiceError::Coordination("cancellation deadline overflow"))?;
        let mut heartbeat = Instant::now();
        let mut observed = None;
        loop {
            if Instant::now() >= deadline
                || self.expired.load(Ordering::Acquire)
                || self.stopping.load(Ordering::Acquire)
            {
                return Err(ServiceError::Coordination(
                    "capture cancellation cleanup incomplete",
                ));
            }
            let mode = self
                .cancellation_mode(store, fence)?
                .ok_or(ServiceError::Coordination("capture cancellation absent"))?;
            if observed != Some(mode) {
                store.request_cancel(&fence.session_id, mode)?;
                let _ = self.cancel.send(mode);
                observed = Some(mode);
            }
            if Instant::now() >= heartbeat {
                let next = Instant::now() + self.budget;
                store.heartbeat(fence)?;
                if self.expired.load(Ordering::Acquire) {
                    return Err(ServiceError::Coordination(
                        "cancelled capture lease expired",
                    ));
                }
                self.renew
                    .send(Some(next))
                    .map_err(|_| ServiceError::Coordination("fresh watchdog stopped"))?;
                heartbeat = Instant::now() + self.options.lease.poll_interval;
            }
            if !*cleanup {
                match self.requests.try_recv() {
                    Ok(Job::Cancelled(stop)) => {
                        publication.check_cancelled(fence, &stop)?;
                        *cleanup = true;
                        store.request_cancel(&fence.session_id, stop.mode())?;
                        let _cleanup = stop.into_cleanup();
                    }
                    Ok(Job::Capture(prepared)) => {
                        publication.check_prepared(fence, &prepared)?;
                        *cleanup = true;
                    }
                    Ok(Job::ActionRequired(stop)) => {
                        publication.check_action_required(fence, &stop)?;
                        *cleanup = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(_) => {
                        return Err(ServiceError::Coordination(
                            "cancelled capture caller disconnected",
                        ));
                    }
                }
            }
            if *cleanup && publication.cancelled_workers_complete()? {
                self.owner_live()?;
                if Instant::now() >= deadline {
                    return Err(ServiceError::Coordination(
                        "capture cancellation cleanup deadline",
                    ));
                }
                if let Some(settings) = &self.snapshot
                    && mode == CancelMode::Graceful
                {
                    // A snapshot worker is another owned process: failed cleanup or
                    // association must not fall through to the ordinary release path.
                    *cleanup = false;
                    super::super::snapshot::attempt(
                        store,
                        fence,
                        settings.clone(),
                        super::super::snapshot::Lease {
                            signals: &self.signals,
                            cancel: &self.cancel,
                            expired: &self.expired,
                            renew: &self.renew,
                            budget: self.budget,
                            options: self.options,
                        },
                    )?;
                    *cleanup = true;
                }
                self.owner_live()?;
                let session = store.finish_attempt(fence, Lifecycle::Suspended, None, None)?;
                *finalized = true;
                self.reply
                    .try_send(Ok(FreshCaptureReply::Cancelled(Box::new(session))))
                    .map_err(|_| ServiceError::Coordination("fresh cancellation reply lost"))?;
                return Ok(());
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
}
