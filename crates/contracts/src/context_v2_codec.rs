use super::*;
use crate::context::{MAX_BASE64_BYTES, MAX_FORMAT_BYTES, MAX_PAYLOAD_BYTES};
use base64::{Engine, engine::general_purpose::STANDARD};

pub fn decode_frame(bytes: &[u8]) -> Result<LoadingFrame, ContextError> {
    if bytes.len() > crate::MAX_FRAME_BYTES {
        return Err(ContextError::Limit);
    }
    let wire = serde_json::from_slice(bytes).map_err(|_| ContextError::Schema)?;
    LoadingFrame::checked(wire)
}
pub fn encode_frame(frame: &LoadingFrame) -> Result<Vec<u8>, ContextError> {
    validate(&frame.0)?;
    bounded(&frame.0, crate::MAX_FRAME_BYTES)
}
pub fn restore_binding(
    scope: &LoadingScope,
    descriptor: &RestoreDescriptor,
) -> Result<ContextDigest, ContextError> {
    payload_length(descriptor.payload_length)?;
    let mut bytes = Vec::new();
    for part in [
        b"foundry.restore.begin-wire.v2".to_vec(),
        bounded(scope, MAX_FORMAT_BYTES)?,
        bounded(descriptor, MAX_FORMAT_BYTES)?,
    ] {
        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
        bytes.extend_from_slice(&part);
    }
    Ok(ContextDigest::of(&bytes))
}
fn payload_length(length: u64) -> Result<(), ContextError> {
    if length == 0 || length > MAX_PAYLOAD_BYTES as u64 {
        return Err(ContextError::Limit);
    }
    Ok(())
}
fn continuation(identity: &ContinuationIdentity) -> Result<(), ContextError> {
    if identity.input_sequence == 0
        || identity.input_sequence > i64::MAX as u64
        || identity.input_bytes == 0
        || identity.input_bytes > crate::MAX_PROMPT_BYTES as u64
    {
        return Err(ContextError::Limit);
    }
    Ok(())
}
pub(super) fn input(identity: &ContinuationIdentity, text: &str) -> Result<(), ContextError> {
    continuation(identity)?;
    if text.trim().is_empty() || text.len() > crate::MAX_PROMPT_BYTES {
        return Err(ContextError::Limit);
    }
    if text.len() as u64 != identity.input_bytes
        || ContextDigest::of(text.as_bytes()) != identity.input_digest
    {
        return Err(ContextError::Integrity);
    }
    Ok(())
}
pub(super) fn payload(encoded: &str) -> Result<ContextPayload, ContextError> {
    if encoded.is_empty() || encoded.len() > MAX_BASE64_BYTES {
        return Err(ContextError::Limit);
    }
    let decoded = STANDARD
        .decode(encoded)
        .map_err(|_| ContextError::Encoding)?;
    if STANDARD.encode(&decoded) != encoded {
        return Err(ContextError::Encoding);
    }
    ContextPayload::new(decoded)
}
pub(super) fn validate(wire: &Wire) -> Result<(), ContextError> {
    match wire {
        Wire::Begin { descriptor, .. } => {
            payload_length(descriptor.payload_length)?;
            bounded(descriptor, MAX_FORMAT_BYTES)?;
        }
        Wire::Payload {
            payload_length: length,
            payload_digest,
            payload_base64,
            ..
        } => {
            payload_length(*length)?;
            let decoded = payload(payload_base64)?;
            if decoded.as_bytes().len() as u64 != *length || decoded.digest() != *payload_digest {
                return Err(ContextError::Integrity);
            }
        }
        Wire::Continue {
            continuation, text, ..
        } => input(continuation, text)?,
        Wire::Accepted {
            continuation: identity,
            ..
        } => continuation(identity)?,
        _ => {}
    }
    Ok(())
}
fn bounded(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, ContextError> {
    struct Buffer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Buffer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut output, value).map_err(|_| ContextError::Limit)?;
    Ok(output.bytes)
}
