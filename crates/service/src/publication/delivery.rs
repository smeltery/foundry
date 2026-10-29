use super::*;
use std::sync::atomic::Ordering;

impl Publication {
    pub(super) fn deliver(&self, store: &mut Store, fence: &AttemptFence) -> Result<()> {
        if self.stage != 1 || !self.control.signals.accepting.load(Ordering::Acquire) {
            return Ok(());
        }
        self.live(store, fence)?;
        submit(store, fence, &self.control.signals, &self.messages)
    }
}

fn submit(
    store: &mut Store,
    fence: &AttemptFence,
    signals: &crate::control::RunControl,
    messages: &mpsc::SyncSender<foundry_runtime::AdapterMessage>,
) -> Result<()> {
    let Some(_permit) = signals.admission.try_claim() else {
        return Ok(());
    };
    // Store permits only one in-flight instruction. Its durable acknowledgement
    // follows runtime dequeue, so the single queue slot is available for a claim.
    if let Some(message) = store.claim_next_message(fence)? {
        messages
            .try_send(foundry_runtime::AdapterMessage {
                message_id: message.id,
                text: message.text,
            })
            .map_err(|_| ServiceError::Coordination("capture message delivery unavailable"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundry_contracts::{CreateSession, MessageState};
    use std::sync::atomic::AtomicU8;

    #[test]
    fn queued_inputs_keep_identity_order_and_stop_at_frozen_gate() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(directory.path().join("db")).unwrap();
        let session = store
            .create(CreateSession {
                prompt: "prompt".into(),
                directory: directory.path().display().to_string(),
                model: None,
                idempotency_key: None,
            })
            .unwrap();
        let fence = store.begin_attempt(&session.id, 0).unwrap();
        for id in ["first", "second", "third"] {
            store.enqueue_message(&session.id, id, "same text").unwrap();
        }
        let signals = crate::control::RunControl::new(Arc::new(AtomicU8::new(0)));
        let (sender, receiver) = mpsc::sync_channel(1);
        submit(&mut store, &fence, &signals, &sender).unwrap();
        // A second poll cannot claim another input until the first is acknowledged.
        submit(&mut store, &fence, &signals, &sender).unwrap();
        let first = receiver.try_recv().unwrap();
        assert_eq!(first.message_id, "first");
        assert!(receiver.try_recv().is_err());
        store.acknowledge_message(&fence, "first").unwrap();
        submit(&mut store, &fence, &signals, &sender).unwrap();
        assert_eq!(receiver.try_recv().unwrap().message_id, "second");
        store.acknowledge_message(&fence, "second").unwrap();
        signals.stop_admission();
        submit(&mut store, &fence, &signals, &sender).unwrap();
        assert!(receiver.try_recv().is_err());
        let messages = store.messages(&session.id, 0, 10).unwrap();
        assert_eq!(messages[2].state, MessageState::Pending);
        assert!(messages[2].attempt_id.is_none());
    }
}
