use super::*;

impl PeerSession {
    /// Caller persists its outbound wire message before calling this; the pending
    /// correlation here is volatile and never marks an outbox delivered. Reconnect
    /// may resend the same immutable message after a lost receipt ACK.
    pub async fn send_text(&mut self, message: wire::Message) -> Result<()> {
        self.begin()?;
        let result = async {
            if self.pending.len() >= MAX_PENDING || self.pending.contains_key(&message.id) {
                return Err(PeerError::Limit);
            }
            let permit = self.permit().await?;
            frames::message(&message, &permit.local, &permit.remote)?;
            let bytes = serde_json::to_vec(&message).map_err(|_| PeerError::Invalid)?;
            let digest = *blake3::hash(&bytes).as_bytes();
            // An ID-only ACK cannot distinguish replaced bytes on this stream.
            // Exact-byte dedup retries are equivalent; replacements need a new
            // connection before another pending owner can be installed.
            if let Some(previous) = self.sent_ids.get(&message.id) {
                if *previous != digest {
                    return Err(PeerError::StaleEpoch);
                }
            } else if self.sent_ids.len() >= crate::mail_store::MAX_MAIL_ROWS {
                return Err(PeerError::Limit);
            }
            io::write(
                &mut self.send,
                &PeerFrame::Msg {
                    msg: Box::new(message.clone()),
                },
                &mut self.options,
            )
            .await?;
            self.sent_ids.insert(message.id.clone(), digest);
            self.pending.insert(message.id, permit.revision);
            Ok(())
        }
        .await;
        self.completed(result)
    }
}
