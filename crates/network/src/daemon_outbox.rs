//! Per-connection receipt ownership. The supervisor owns transport shutdown and
//! must join the old driver before creating a ledger for its replacement.
use crate::{
    mail_store::{
        MAX_MAIL_ROWS, MailError, MailStore, TransportAttempt, TransportDisposition,
        TransportMessage,
    },
    wire,
};
use iroh::EndpointId;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

pub(crate) const RECEIPT_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) enum QueueAction {
    Pending,
    Retire,
    Send(Box<wire::Message>),
}
pub(crate) enum Settlement {
    Settled { taken: bool },
    Retire,
}
struct SentVersion {
    team: String,
    revision: u64,
}
struct PendingAttempt {
    attempt: TransportAttempt,
    started: Instant,
}
/// No Debug/Serialize: attempts retain message contents and wire identifiers.
pub(crate) struct OutboxLedger {
    peer: EndpointId,
    generation: u64,
    inflight_limit: usize,
    sent: BTreeMap<String, SentVersion>,
    pending: BTreeMap<String, PendingAttempt>,
    retiring: bool,
}
impl OutboxLedger {
    pub(crate) fn new(
        peer: EndpointId,
        generation: u64,
        inflight_limit: usize,
    ) -> Result<Self, MailError> {
        if inflight_limit == 0 || inflight_limit > MAX_MAIL_ROWS {
            return Err(MailError::InvalidInput);
        }
        Ok(Self {
            peer,
            generation,
            inflight_limit,
            sent: BTreeMap::new(),
            pending: BTreeMap::new(),
            retiring: false,
        })
    }
    /// Inspect globally, including replacements redirected to a different peer.
    /// A terminal receipt may remove a row, but its sent-ID history stays until
    /// this connection is joined: a future enqueue may reuse that identifier.
    /// Scan each live ledger before dispatching a batch across peer targets.
    pub(crate) fn requires_retirement(&mut self, store: &MailStore) -> Result<bool, MailError> {
        if self.retiring {
            return Ok(true);
        }
        for (id, sent) in &self.sent {
            if let Some(current) = store.transport_message(id)?
                && (current.peer != self.peer
                    || current.team != sent.team
                    || current.revision != sent.revision)
            {
                self.retiring = true;
                break;
            }
        }
        Ok(self.retiring)
    }
    /// Application receipts have a deadline even when the transport stays live.
    /// Expiry requires retiring and joining the driver, not reusing an ID-only
    /// acknowledgement stream with a fresh indistinguishable attempt.
    pub(crate) fn receipt_deadline_elapsed(&self, now: Instant) -> bool {
        self.pending
            .values()
            .any(|pending| now.saturating_duration_since(pending.started) >= RECEIPT_TIMEOUT)
    }
    fn check_id(&mut self, store: &MailStore, id: &str) -> Result<bool, MailError> {
        if !self.retiring
            && let Some(sent) = self.sent.get(id)
            && let Some(current) = store.transport_message(id)?
        {
            self.retiring = current.peer != self.peer
                || current.team != sent.team
                || current.revision != sent.revision;
        }
        Ok(self.retiring)
    }
    /// Persist attempt ownership before returning bytes for a network write.
    /// Any write failure requires driver retirement; do not reuse this ledger.
    pub(crate) fn prepare(
        &mut self,
        store: &mut MailStore,
        queued: &TransportMessage,
    ) -> Result<QueueAction, MailError> {
        if self.check_id(store, &queued.message.id)? {
            return Ok(QueueAction::Retire);
        }
        if queued.peer != self.peer {
            return Err(MailError::InvalidInput);
        }
        if self.pending.contains_key(&queued.message.id)
            || self.pending.len() >= self.inflight_limit
        {
            return Ok(QueueAction::Pending);
        }
        if let Some(sent) = self.sent.get(&queued.message.id) {
            if sent.team != queued.team || sent.revision != queued.revision {
                self.retiring = true;
                return Ok(QueueAction::Retire);
            }
        } else if self.sent.len() >= MAX_MAIL_ROWS {
            self.retiring = true;
            return Ok(QueueAction::Retire);
        }
        let attempt = store.begin_transport_attempt(queued, self.generation)?;
        let id = queued.message.id.clone();
        self.sent.insert(
            id.clone(),
            SentVersion {
                team: queued.team.clone(),
                revision: queued.revision,
            },
        );
        self.pending.insert(
            id,
            PendingAttempt {
                attempt,
                started: Instant::now(),
            },
        );
        Ok(QueueAction::Send(Box::new(queued.message.clone())))
    }
    pub(crate) fn settle(
        &mut self,
        store: &mut MailStore,
        id: &str,
        disposition: TransportDisposition,
    ) -> Result<Settlement, MailError> {
        if self.check_id(store, id)? {
            return Ok(Settlement::Retire);
        }
        let pending = self.pending.get(id).ok_or(MailError::StaleAttempt)?;
        let taken = disposition.taken();
        match store.finish_transport_attempt(
            &pending.attempt,
            self.peer,
            self.generation,
            disposition,
        ) {
            Ok(()) => {
                self.pending.remove(id);
                Ok(Settlement::Settled { taken })
            }
            Err(MailError::StaleAttempt) => {
                self.retiring = true;
                Ok(Settlement::Retire)
            }
            Err(error) => Err(error),
        }
    }
    /// Call after stopping and joining the actual driver. Resets only attempts
    /// from this connection generation; durable queued envelopes are preserved.
    pub(crate) fn disconnected(self, store: &mut MailStore) -> Result<(), MailError> {
        store.disconnect_transport(self.peer, self.generation)
    }
}

#[cfg(test)]
#[path = "daemon_outbox_tests.rs"]
mod tests;
