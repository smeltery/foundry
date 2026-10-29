//! Release only after actual runtime cleanup or before exposing the runtime.
use super::*;
use foundry_store::RestoreClose;

impl Actor {
    pub(super) fn release(
        &self,
        store: &mut Store,
        reservation: &Reservation,
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
            let lifecycle = if store.cancellation(&grant.fence)?.is_some() {
                foundry_contracts::Lifecycle::Suspended
            } else {
                foundry_contracts::Lifecycle::Error
            };
            store.finish_attempt(&grant.fence, lifecycle, None, None)?;
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
