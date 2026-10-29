//! Release only after actual runtime cleanup or before exposing the runtime.
use super::*;
use foundry_store::RestoreClose;

impl Actor {
    pub(super) fn release(
        &self,
        store: &mut Store,
        reservation: &mut Reservation,
        failed: bool,
    ) -> Result<Option<RestoreRef>> {
        if reservation.attempt_finalized {
            return Ok(None);
        }
        if reservation.exposed {
            let observer = reservation
                .cleanup
                .as_ref()
                .ok_or(ServiceError::Coordination(
                    "runtime cleanup unverified; restore retained",
                ))?;
            let deadline = Instant::now() + self.options.cleanup_timeout;
            let mut heartbeat = Instant::now();
            loop {
                match observer.status() {
                    CleanupStatus::Complete => break,
                    CleanupStatus::Failed => {
                        return Err(ServiceError::Coordination(
                            "runtime cleanup failed; restore retained",
                        ));
                    }
                    CleanupStatus::Pending => {}
                }
                if Instant::now() >= deadline || self.io.expired.load(Ordering::Acquire) {
                    return Err(ServiceError::Coordination(
                        "runtime cleanup incomplete; restore retained",
                    ));
                }
                if Instant::now() >= heartbeat {
                    self.heartbeat(store, reservation)?;
                    heartbeat = Instant::now() + self.options.lease.poll_interval;
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
        if self.io.expired.load(Ordering::Acquire) {
            return Err(ServiceError::Coordination(
                "restore deadline elapsed; expiry recovery required",
            ));
        }
        if let Some(mode) = self.io.signals.relay(&self.io.cancel) {
            store.request_cancel(&reservation.session, mode)?;
        }
        if let Some(grant) = reservation
            .continuation
            .as_ref()
            .and_then(|link| link.grant.as_ref())
        {
            let cancelled = store.cancellation(&grant.fence)?;
            if cancelled == Some(CancelMode::Graceful)
                && let Some(settings) = &self.snapshot_settings
            {
                if let Some(publication) = reservation.publication.as_mut() {
                    let deadline = Instant::now() + self.options.cleanup_timeout;
                    while !publication.cancelled_workers_complete()? {
                        if Instant::now() >= deadline || self.io.expired.load(Ordering::Acquire) {
                            return Err(ServiceError::Coordination(
                                "restored publication cleanup incomplete",
                            ));
                        }
                        let next = Instant::now() + self.budget;
                        store.heartbeat(&grant.fence)?;
                        self.io
                            .renew
                            .send(Some(next))
                            .map_err(|_| ServiceError::Coordination("restore watchdog stopped"))?;
                        thread::sleep(self.options.lease.poll_interval);
                    }
                }
                crate::fresh_capture_owner::snapshot::attempt(
                    store,
                    &grant.fence,
                    settings.clone(),
                    crate::fresh_capture_owner::snapshot::Lease {
                        signals: &self.io.signals,
                        cancel: &self.io.cancel,
                        expired: &self.io.expired,
                        renew: &self.io.renew,
                        budget: self.budget,
                        options: self.options,
                    },
                )?;
            }
            let lifecycle = if cancelled.is_some() {
                foundry_contracts::Lifecycle::Suspended
            } else {
                foundry_contracts::Lifecycle::Error
            };
            let session = store.finish_attempt(&grant.fence, lifecycle, None, None)?;
            if lifecycle == foundry_contracts::Lifecycle::Suspended
                && let Some(acceptance) = &grant.acceptance
                && acceptance.attempt_id == grant.fence.attempt_id
                && acceptance.session_id == session.id
                && let Some(cleanup) = &reservation.cleanup
                && cleanup.status() == CleanupStatus::Complete
            {
                reservation.cancellation = Some(RestoredCancellation {
                    session,
                    attempt: grant.fence.attempt_id.clone(),
                    message: acceptance.message_id.clone(),
                    scope: reservation.scope.clone(),
                    cleanup: cleanup.clone(),
                    capture: reservation
                        .publication
                        .as_ref()
                        .map(|publication| publication.cancellation_matcher()),
                });
            }
            return Ok(None);
        }
        Ok(Some(store.close_restore(
            &reservation.fence,
            if failed {
                RestoreClose::Failed
            } else {
                RestoreClose::Closed
            },
        )?))
    }
}
