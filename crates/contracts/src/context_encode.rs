use super::*;

/// Explicit JSON encoding, excluding the JSONL newline. Payload/text receipts
/// serialize last. No context type is accepted by generic AdapterEvent persistence.
pub fn encode_command(command: &ContextCommand) -> Result<Vec<u8>, ContextError> {
    let wire = match command {
        ContextCommand::CaptureBegin {
            attempt_id,
            capture_id,
            format_digest,
            configuration_digest,
            challenge,
        } => Wire::CaptureBegin {
            attempt_id: attempt_id.clone(),
            capture_id: capture_id.clone(),
            format_digest: *format_digest,
            configuration_digest: *configuration_digest,
            challenge: challenge.clone(),
        },
        ContextCommand::Export {
            attempt_id,
            capture_id,
            format_digest,
            cut_digest,
            challenge,
        } => Wire::Export {
            attempt_id: attempt_id.clone(),
            capture_id: capture_id.clone(),
            format_digest: *format_digest,
            cut_digest: *cut_digest,
            challenge: challenge.clone(),
        },
        ContextCommand::Release {
            attempt_id,
            operation_id,
        } => Wire::Release {
            attempt_id: attempt_id.clone(),
            operation_id: operation_id.clone(),
        },
        ContextCommand::RestoreBegin {
            attempt_id,
            restore_id,
            checkpoint_id,
            record_digest,
            format,
            configuration_digest,
            payload_length: length,
            payload_digest,
        } => {
            payload_length(*length)?;
            metadata(format, None)?;
            Wire::RestoreBegin {
                attempt_id: attempt_id.clone(),
                restore_id: restore_id.clone(),
                checkpoint_id: checkpoint_id.clone(),
                record_digest: *record_digest,
                format: format.clone(),
                configuration_digest: *configuration_digest,
                payload_length: *length,
                payload_digest: *payload_digest,
            }
        }
        ContextCommand::RestorePayload {
            attempt_id,
            restore_id,
            payload,
            receipt,
        } => Wire::RestorePayload {
            attempt_id: attempt_id.clone(),
            restore_id: restore_id.clone(),
            payload_base64: payload.encoded(),
            receipt: receipt.clone(),
        },
        ContextCommand::Continue {
            attempt_id,
            restore_id,
            continuation_id,
            input_id,
            input_digest,
            text,
            receipt,
        } => {
            input(text, *input_digest)?;
            Wire::Continue {
                attempt_id: attempt_id.clone(),
                restore_id: restore_id.clone(),
                continuation_id: continuation_id.clone(),
                input_id: input_id.clone(),
                input_digest: *input_digest,
                text: text.clone(),
                receipt: receipt.clone(),
            }
        }
        ContextCommand::Failed {
            attempt_id,
            operation_id,
            code,
        } => Wire::Failed {
            attempt_id: attempt_id.clone(),
            operation_id: operation_id.clone(),
            code: *code,
        },
    };
    bounded_json(&wire, crate::MAX_FRAME_BYTES)
}

pub fn encode_event(event: &ContextEvent) -> Result<Vec<u8>, ContextError> {
    let wire = match event {
        ContextEvent::CaptureReady {
            attempt_id,
            capture_id,
            format_digest,
            configuration_digest,
            challenge,
        } => Wire::CaptureReady {
            attempt_id: attempt_id.clone(),
            capture_id: capture_id.clone(),
            format_digest: *format_digest,
            configuration_digest: *configuration_digest,
            challenge: challenge.clone(),
        },
        ContextEvent::Candidate {
            attempt_id,
            capture_id,
            challenge,
            format,
            coverage,
            payload,
        } => {
            metadata(format, Some(coverage))?;
            Wire::Candidate {
                attempt_id: attempt_id.clone(),
                capture_id: capture_id.clone(),
                challenge: challenge.clone(),
                format: format.clone(),
                coverage: coverage.clone(),
                payload_length: payload.as_bytes().len() as u64,
                payload_digest: payload.digest(),
                payload_base64: payload.encoded(),
            }
        }
        ContextEvent::RestorePrepared {
            attempt_id,
            restore_id,
            format_digest,
            configuration_digest,
        } => Wire::RestorePrepared {
            attempt_id: attempt_id.clone(),
            restore_id: restore_id.clone(),
            format_digest: *format_digest,
            configuration_digest: *configuration_digest,
        },
        ContextEvent::Restored {
            attempt_id,
            restore_id,
            payload_digest,
            format_digest,
            configuration_digest,
            receipt,
        } => Wire::Restored {
            attempt_id: attempt_id.clone(),
            restore_id: restore_id.clone(),
            payload_digest: *payload_digest,
            format_digest: *format_digest,
            configuration_digest: *configuration_digest,
            receipt: receipt.clone(),
        },
        ContextEvent::ContinuationAccepted {
            attempt_id,
            restore_id,
            continuation_id,
            input_id,
            input_digest,
            receipt,
        } => Wire::ContinuationAccepted {
            attempt_id: attempt_id.clone(),
            restore_id: restore_id.clone(),
            continuation_id: continuation_id.clone(),
            input_id: input_id.clone(),
            input_digest: *input_digest,
            receipt: receipt.clone(),
        },
        ContextEvent::Failed {
            attempt_id,
            operation_id,
            code,
        } => Wire::Failed {
            attempt_id: attempt_id.clone(),
            operation_id: operation_id.clone(),
            code: *code,
        },
    };
    bounded_json(&wire, crate::MAX_FRAME_BYTES)
}
