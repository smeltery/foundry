//! Session cancellation as reported to callers.
use crate::{Result, Service};
use foundry_contracts::{CancelMode, CancelReceipt};

impl Service {
    /// Record a cancel and report its effect. Cancelling a running attempt is
    /// asynchronous: the receipt still says `running`, and the owner settles the
    /// attempt as `suspended` without publishing a checkpoint. Hard upgrades a
    /// graceful request and never downgrades. Cancelling a suspended session
    /// makes it terminally `cancelled`; later admission, restoration and
    /// follow-up messages are refused with `ContractError::Cancelled`.
    pub fn cancel(&mut self, id: &str, mode: CancelMode) -> Result<CancelReceipt> {
        Ok(self.store.cancel_session(id, mode)?.receipt)
    }
}
