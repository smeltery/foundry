use super::*;
use crate::peer_session::{PeerError, ReceiptDisposition};
use std::collections::VecDeque;

struct Receipt {
    id: String,
    disposition: ReceiptDisposition,
    revision: u64,
}
#[derive(Default)]
pub(super) struct Slot {
    closed: bool,
    work: VecDeque<Receipt>,
}
pub(super) fn publisher(slot: Arc<Mutex<Slot>>) -> ReceiptPublisher {
    Arc::new(move |id, disposition, revision| {
        // Called inside the ACK's authority permit: bounded memory work only.
        let mut slot = slot.lock().map_err(|_| PeerError::Storage)?;
        if slot.closed {
            return Err(PeerError::Closed);
        }
        if slot.work.len() >= crate::peer_session::MAX_PENDING {
            return Err(PeerError::Limit);
        }
        slot.work.push_back(Receipt {
            id: id.into(),
            disposition: disposition.clone(),
            revision,
        });
        Ok(())
    })
}
impl Store {
    pub(super) fn close_selected(&self, keys: &[(EndpointId, u64)]) -> Result<(), ControlError> {
        for key in keys {
            let mut slot = self
                .receipts
                .get(key)
                .ok_or(ControlError::Storage)?
                .lock()
                .map_err(|_| ControlError::Storage)?;
            slot.closed = true;
            slot.work.clear();
        }
        Ok(())
    }
    pub(super) fn close_receipts(&self) -> Result<(), ControlError> {
        self.close_selected(&self.receipts.keys().copied().collect::<Vec<_>>())
    }
    pub(super) fn drain(
        &mut self,
        key: (EndpointId, u64),
        revision: u64,
        active: bool,
    ) -> Result<bool, ControlError> {
        // The caller retains the actual authority gate. No publisher can race
        // this drain or the immediately following expiry decision on that gate.
        let mut slot = self
            .receipts
            .get(&key)
            .ok_or(ControlError::Storage)?
            .lock()
            .map_err(|_| ControlError::Storage)?;
        if slot.closed || !active || slot.work.iter().any(|r| r.revision != revision) {
            slot.closed = true;
            slot.work.clear();
            return Ok(true);
        }
        let ledger = self.ledgers.get_mut(&key).ok_or(ControlError::Storage)?;
        while let Some(receipt) = slot.work.pop_front() {
            match ledger
                .settle(&mut self.mail, &receipt.id, receipt.disposition)
                .map_err(|_| ControlError::Storage)?
            {
                Settlement::Settled { taken } => {
                    // Client delivery remains separate from transport settlement.
                    let _taken = taken;
                }
                Settlement::Retire => {
                    slot.closed = true;
                    slot.work.clear();
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
#[path = "outbox_receipts_tests.rs"]
mod tests;
