use crate::{RuntimeError, process::Process};
use foundry_contracts::{AdapterCommand, MAX_PROMPT_BYTES};
use std::{collections::HashSet, sync::mpsc::Receiver};

/// Retained IDs and receipts are never evicted within an attempt.
pub const MAX_MESSAGES_PER_ATTEMPT: usize = 1024;

pub(crate) fn validate_message(attempt: &str, id: &str, text: &str) -> Result<(), RuntimeError> {
    if [attempt, id]
        .iter()
        .any(|id| id.trim().is_empty() || id.len() > 128 || id.chars().any(char::is_control))
    {
        return Err(RuntimeError::Invalid(
            "message and attempt IDs require 1–128 bytes without controls",
        ));
    }
    if text.trim().is_empty() || text.len() > MAX_PROMPT_BYTES {
        return Err(RuntimeError::Invalid(
            "message requires bounded nonempty text",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct AdapterMessage {
    pub message_id: String,
    pub text: String,
}

#[derive(Default)]
pub(crate) struct Delivery {
    pub enabled: bool,
    submitted: HashSet<String>,
}
impl Delivery {
    pub fn forward(
        &mut self,
        messages: &Receiver<AdapterMessage>,
        process: &Process,
        attempt: &str,
    ) -> Result<(), RuntimeError> {
        self.forward_traced(messages, process, attempt, None)
    }
    pub fn forward_traced(
        &mut self,
        messages: &Receiver<AdapterMessage>,
        process: &Process,
        attempt: &str,
        trace: Option<&mut crate::coordinated::capture::DeliveredTrace>,
    ) -> Result<(), RuntimeError> {
        if !self.enabled {
            return Ok(());
        }
        if let Ok(message) = messages.try_recv() {
            validate_message(attempt, &message.message_id, &message.text)?;
            if self.submitted.contains(&message.message_id) {
                return Err(RuntimeError::Invalid(
                    "message was already submitted in this attempt",
                ));
            }
            if self.submitted.len() >= MAX_MESSAGES_PER_ATTEMPT {
                return Err(RuntimeError::Invalid("message limit per attempt exceeded"));
            }
            let reservation = trace
                .map(|trace| trace.reserve_instruction(attempt, &message.message_id, &message.text))
                .transpose()
                .map_err(|_| RuntimeError::Protocol("instruction trace rejected"))?;
            process.send(&AdapterCommand::Message {
                attempt_id: attempt.into(),
                message_id: message.message_id.clone(),
                text: message.text,
                receipt: uuid::Uuid::new_v4().to_string(),
            })?;
            if let Some(reservation) = reservation {
                reservation.commit_enqueued();
            }
            self.submitted.insert(message.message_id);
        }
        Ok(())
    }
    pub fn validate_ack(&self, id: &str) -> Result<(), RuntimeError> {
        if !self.enabled || !self.submitted.contains(id) {
            return Err(RuntimeError::Protocol(
                "acknowledgment does not match a submitted message",
            ));
        }
        Ok(())
    }
}
