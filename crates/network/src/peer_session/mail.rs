use super::*;
use crate::mail_store::{DeliveryIntent, Insert, MailError, MailStore, Message, State};

/// Explicit text-receipt adapter. Caller supplies current recipient resolution
/// (including session/folder visibility and inbound acceptance policy) on EVERY
/// receipt; it returns the local mailbox key or an explicit refusal. No defaults,
/// hidden client detection, approval/file stripping, or external injection occurs.
/// Records contain the complete accepted wire JSON, not just a lossy text body.
type RecipientResolver = dyn Fn(&str) -> Result<String> + Send;

pub struct TextMailbox {
    store: MailStore,
    resolve: Box<RecipientResolver>,
    arrived: Option<Arc<tokio::sync::Notify>>,
}
impl TextMailbox {
    pub fn new(
        store: MailStore,
        resolve: impl Fn(&str) -> Result<String> + Send + 'static,
    ) -> Self {
        Self {
            store,
            resolve: Box::new(resolve),
            arrived: None,
        }
    }
    /// Notify after durable insertion, before any ACK network output. A broken
    /// writer or lost driver event cannot hide already accepted inbox mail.
    pub fn with_notification(mut self, arrived: Arc<tokio::sync::Notify>) -> Self {
        self.arrived = Some(arrived);
        self
    }
    pub(super) fn receive(&mut self, message: &wire::Message) -> Result<bool> {
        let recipient = (self.resolve)(&message.to)?;
        let body = serde_json::to_vec(message).map_err(|_| PeerError::Invalid)?;
        let record = Message {
            id: message.id.clone(),
            sender: message.from.clone(),
            recipient,
            body,
            encoding: crate::mail_store::PayloadEncoding::WireJson,
            reply_to: message.reply_to.clone(),
            intent: DeliveryIntent {
                wake: message.wake,
                notify: message.notify,
            },
        };
        let inserted = self
            .store
            .insert(&record, State::Pending)
            .map_err(|error| match error {
                MailError::Conflict => PeerError::Invalid,
                MailError::Capacity => PeerError::Limit,
                MailError::InvalidInput | MailError::InvalidPayload => PeerError::Invalid,
                MailError::UnsupportedPayload => PeerError::Unsupported,
                _ => PeerError::Storage,
            })?;
        if let Some(arrived) = &self.arrived {
            arrived.notify_waiters();
        }
        Ok(inserted == Insert::Duplicate)
    }
}
