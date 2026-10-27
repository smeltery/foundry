//! Bounded message receipt registration before pipe enqueue.
use super::*;

#[cfg(all(test, unix))]
#[path = "process_messages_tests.rs"]
mod tests;

impl Process {
    pub fn send(&self, command: &AdapterCommand) -> Result<(), RuntimeError> {
        if self.loading_scope.is_some() {
            return Err(RuntimeError::Protocol("ordinary command during loading"));
        }
        if let AdapterCommand::Message {
            attempt_id,
            message_id,
            text,
            receipt,
        } = command
        {
            crate::delivery::validate_message(attempt_id, message_id, text)?;
            if receipt.trim().is_empty()
                || receipt.len() > 128
                || receipt.chars().any(char::is_control)
            {
                return Err(RuntimeError::Invalid(
                    "message receipt requires 1–128 bytes without controls",
                ));
            }
        }
        let sender = self.commands.as_ref().ok_or(RuntimeError::CommandQueue)?;
        let mut bytes = serde_json::to_vec(command)
            .map_err(|_| RuntimeError::Protocol("command serialization failed"))?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(RuntimeError::FrameTooLarge);
        }
        bytes.push(b'\n');
        let message = match command {
            AdapterCommand::Message {
                attempt_id,
                message_id,
                receipt,
                ..
            } => Some(((attempt_id.clone(), message_id.clone()), receipt.clone())),
            _ => None,
        };
        // Register before enqueueing: a genuine peer can acknowledge before the
        // input worker resumes from write_all. Never lock across pipe I/O.
        if let Some((key, receipt)) = &message {
            let mut receipts = self
                .receipts
                .lock()
                .map_err(|_| RuntimeError::Protocol("message tracking unavailable"))?;
            if receipts.contains_key(key) {
                return Err(RuntimeError::Invalid(
                    "message was already submitted in this attempt",
                ));
            }
            if receipts.len() >= crate::MAX_MESSAGES_PER_ATTEMPT {
                return Err(RuntimeError::Invalid("message limit per attempt exceeded"));
            }
            receipts.insert(key.clone(), receipt.clone());
        }
        if sender.try_send(InputFrame { bytes }).is_err() {
            if let Some((key, _)) = message {
                self.receipts
                    .lock()
                    .map_err(|_| RuntimeError::Protocol("message tracking unavailable"))?
                    .remove(&key);
            }
            return Err(RuntimeError::CommandQueue);
        }
        Ok(())
    }
}

impl Process {
    /// Only trace reservations supply raw result bytes; callers cannot inject frames.
    pub(crate) fn send_reserved_result(
        &self,
        attempt: &str,
        call: &str,
        reservation: &crate::coordinated::capture::TraceReservation<'_>,
    ) -> Result<(), RuntimeError> {
        if self.loading_scope.is_some() {
            return Err(RuntimeError::Protocol("tool result during loading"));
        }
        crate::delivery::validate_message(attempt, call, "result")?;
        let attempt =
            serde_json::to_string(attempt).map_err(|_| RuntimeError::Invalid("invalid attempt"))?;
        let call =
            serde_json::to_string(call).map_err(|_| RuntimeError::Invalid("invalid call"))?;
        let prefix = format!(
            "{{\"type\":\"tool_result\",\"attempt_id\":{attempt},\"call_id\":{call},\"result\":"
        );
        if prefix.len() + reservation.bytes().len() + 1 > MAX_FRAME_BYTES {
            return Err(RuntimeError::FrameTooLarge);
        }
        let mut bytes = prefix.into_bytes();
        bytes.extend_from_slice(reservation.bytes());
        bytes.extend_from_slice(b"}\n");
        self.commands
            .as_ref()
            .ok_or(RuntimeError::CommandQueue)?
            .try_send(InputFrame { bytes })
            .map_err(|_| RuntimeError::CommandQueue)
    }
}
