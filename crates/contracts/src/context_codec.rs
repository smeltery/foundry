use super::*;
use fields::{bounded_json, metadata};
#[path = "context_encode.rs"]
mod encode;
pub use encode::{encode_command, encode_event};

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Wire {
    #[serde(rename = "context_capture_ready")]
    CaptureReady {
        attempt_id: ContextId,
        capture_id: ContextId,
        format_digest: ContextDigest,
        configuration_digest: ContextDigest,
        challenge: ContextReceipt,
    },
    #[serde(rename = "context_capture_begin")]
    CaptureBegin {
        attempt_id: ContextId,
        capture_id: ContextId,
        format_digest: ContextDigest,
        configuration_digest: ContextDigest,
        challenge: ContextReceipt,
    },
    #[serde(rename = "context_export")]
    Export {
        attempt_id: ContextId,
        capture_id: ContextId,
        format_digest: ContextDigest,
        cut_digest: ContextDigest,
        challenge: ContextReceipt,
    },
    #[serde(rename = "context_release")]
    Release {
        attempt_id: ContextId,
        operation_id: ContextId,
    },
    #[serde(rename = "restore_begin")]
    RestoreBegin {
        attempt_id: ContextId,
        restore_id: ContextId,
        checkpoint_id: ContextId,
        record_digest: ContextDigest,
        format: ContextFormat,
        configuration_digest: ContextDigest,
        payload_length: u64,
        payload_digest: ContextDigest,
    },
    #[serde(rename = "restore_payload")]
    RestorePayload {
        attempt_id: ContextId,
        restore_id: ContextId,
        payload_base64: String,
        receipt: ContextReceipt,
    },
    #[serde(rename = "continue_context")]
    Continue {
        attempt_id: ContextId,
        restore_id: ContextId,
        continuation_id: ContextId,
        input_id: ContextId,
        input_digest: ContextDigest,
        text: String,
        receipt: ContextReceipt,
    },
    #[serde(rename = "context_candidate")]
    Candidate {
        attempt_id: ContextId,
        capture_id: ContextId,
        challenge: ContextReceipt,
        format: ContextFormat,
        coverage: ContextCoverage,
        payload_length: u64,
        payload_digest: ContextDigest,
        payload_base64: String,
    },
    #[serde(rename = "restore_prepared")]
    RestorePrepared {
        attempt_id: ContextId,
        restore_id: ContextId,
        format_digest: ContextDigest,
        configuration_digest: ContextDigest,
    },
    #[serde(rename = "context_restored")]
    Restored {
        attempt_id: ContextId,
        restore_id: ContextId,
        payload_digest: ContextDigest,
        format_digest: ContextDigest,
        configuration_digest: ContextDigest,
        receipt: ContextReceipt,
    },
    #[serde(rename = "continuation_accepted")]
    ContinuationAccepted {
        attempt_id: ContextId,
        restore_id: ContextId,
        continuation_id: ContextId,
        input_id: ContextId,
        input_digest: ContextDigest,
        receipt: ContextReceipt,
    },
    #[serde(rename = "context_failed")]
    Failed {
        attempt_id: ContextId,
        operation_id: ContextId,
        code: ContextFailure,
    },
}

fn frame(bytes: &[u8]) -> Result<Wire, ContextError> {
    if bytes.len() > crate::MAX_FRAME_BYTES {
        return Err(ContextError::Limit);
    }
    serde_json::from_slice(bytes).map_err(|_| ContextError::Schema)
}
fn payload_length(value: u64) -> Result<(), ContextError> {
    if value == 0 || value > MAX_PAYLOAD_BYTES as u64 {
        return Err(ContextError::Limit);
    }
    Ok(())
}
fn input(text: &str, digest: ContextDigest) -> Result<(), ContextError> {
    if text.trim().is_empty() || text.len() > crate::MAX_PROMPT_BYTES {
        return Err(ContextError::Limit);
    }
    if ContextDigest::of(text.as_bytes()) != digest {
        return Err(ContextError::Integrity);
    }
    Ok(())
}

/// Classification only, not validation. Duplicate type tags fail rather than
/// permitting an unsolicited control frame to masquerade as an ordinary event.
pub fn is_context_frame(bytes: &[u8]) -> Result<bool, ContextError> {
    if bytes.len() > crate::MAX_FRAME_BYTES {
        return Err(ContextError::Limit);
    }
    #[derive(Deserialize)]
    struct Tag {
        #[serde(rename = "type")]
        kind: String,
    }
    let tag: Tag = serde_json::from_slice(bytes).map_err(|_| ContextError::Schema)?;
    Ok(matches!(
        tag.kind.as_str(),
        "context_capture_begin"
            | "context_capture_ready"
            | "context_export"
            | "context_release"
            | "restore_begin"
            | "restore_payload"
            | "continue_context"
            | "context_candidate"
            | "restore_prepared"
            | "context_restored"
            | "continuation_accepted"
            | "context_failed"
    ))
}

pub fn decode_command(bytes: &[u8]) -> Result<ContextCommand, ContextError> {
    Ok(match frame(bytes)? {
        Wire::CaptureBegin {
            attempt_id,
            capture_id,
            format_digest,
            configuration_digest,
            challenge,
        } => ContextCommand::CaptureBegin {
            attempt_id,
            capture_id,
            format_digest,
            configuration_digest,
            challenge,
        },
        Wire::Export {
            attempt_id,
            capture_id,
            format_digest,
            cut_digest,
            challenge,
        } => ContextCommand::Export {
            attempt_id,
            capture_id,
            format_digest,
            cut_digest,
            challenge,
        },
        Wire::Release {
            attempt_id,
            operation_id,
        } => ContextCommand::Release {
            attempt_id,
            operation_id,
        },
        Wire::RestoreBegin {
            attempt_id,
            restore_id,
            checkpoint_id,
            record_digest,
            format,
            configuration_digest,
            payload_length: length,
            payload_digest,
        } => {
            payload_length(length)?;
            metadata(&format, None)?;
            ContextCommand::RestoreBegin {
                attempt_id,
                restore_id,
                checkpoint_id,
                record_digest,
                format,
                configuration_digest,
                payload_length: length,
                payload_digest,
            }
        }
        Wire::RestorePayload {
            attempt_id,
            restore_id,
            payload_base64,
            receipt,
        } => ContextCommand::RestorePayload {
            attempt_id,
            restore_id,
            payload: ContextPayload::decode(&payload_base64)?,
            receipt,
        },
        Wire::Continue {
            attempt_id,
            restore_id,
            continuation_id,
            input_id,
            input_digest,
            text,
            receipt,
        } => {
            input(&text, input_digest)?;
            ContextCommand::Continue {
                attempt_id,
                restore_id,
                continuation_id,
                input_id,
                input_digest,
                text,
                receipt,
            }
        }
        Wire::Failed {
            attempt_id,
            operation_id,
            code,
        } => ContextCommand::Failed {
            attempt_id,
            operation_id,
            code,
        },
        _ => return Err(ContextError::Schema),
    })
}

pub fn decode_event(bytes: &[u8]) -> Result<ContextEvent, ContextError> {
    Ok(match frame(bytes)? {
        Wire::CaptureReady {
            attempt_id,
            capture_id,
            format_digest,
            configuration_digest,
            challenge,
        } => ContextEvent::CaptureReady {
            attempt_id,
            capture_id,
            format_digest,
            configuration_digest,
            challenge,
        },
        Wire::Candidate {
            attempt_id,
            capture_id,
            challenge,
            format,
            coverage,
            payload_length: length,
            payload_digest,
            payload_base64,
        } => {
            payload_length(length)?;
            metadata(&format, Some(&coverage))?;
            let payload = ContextPayload::decode(&payload_base64)?;
            if payload.as_bytes().len() as u64 != length || payload.digest() != payload_digest {
                return Err(ContextError::Integrity);
            }
            ContextEvent::Candidate {
                attempt_id,
                capture_id,
                challenge,
                format,
                coverage,
                payload,
            }
        }
        Wire::RestorePrepared {
            attempt_id,
            restore_id,
            format_digest,
            configuration_digest,
        } => ContextEvent::RestorePrepared {
            attempt_id,
            restore_id,
            format_digest,
            configuration_digest,
        },
        Wire::Restored {
            attempt_id,
            restore_id,
            payload_digest,
            format_digest,
            configuration_digest,
            receipt,
        } => ContextEvent::Restored {
            attempt_id,
            restore_id,
            payload_digest,
            format_digest,
            configuration_digest,
            receipt,
        },
        Wire::ContinuationAccepted {
            attempt_id,
            restore_id,
            continuation_id,
            input_id,
            input_digest,
            receipt,
        } => ContextEvent::ContinuationAccepted {
            attempt_id,
            restore_id,
            continuation_id,
            input_id,
            input_digest,
            receipt,
        },
        Wire::Failed {
            attempt_id,
            operation_id,
            code,
        } => ContextEvent::Failed {
            attempt_id,
            operation_id,
            code,
        },
        _ => return Err(ContextError::Schema),
    })
}
