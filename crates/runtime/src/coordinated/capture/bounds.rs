use super::CaptureError;
use serde::Serialize;
use std::io::Write;

pub(super) fn identifier(value: &str) -> Result<(), CaptureError> {
    if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(CaptureError::Invalid);
    }
    Ok(())
}

pub(super) fn measure(value: &impl Serialize, limit: usize) -> Result<usize, CaptureError> {
    struct Counter {
        used: usize,
        limit: usize,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.used) {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            self.used += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { used: 0, limit };
    serde_json::to_writer(&mut counter, value).map_err(|_| CaptureError::Limit)?;
    Ok(counter.used)
}

pub(super) fn json(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, CaptureError> {
    let length = measure(value, limit)?;
    let mut bytes = Vec::with_capacity(length);
    serde_json::to_writer(&mut bytes, value).map_err(|_| CaptureError::Invalid)?;
    Ok(bytes)
}

/// Versioned length-delimited private encoding, not the store's request digest.
pub(super) struct Encoding {
    bytes: Vec<u8>,
    limit: usize,
}
impl Encoding {
    pub fn new(domain: &[u8], limit: usize) -> Result<Self, CaptureError> {
        let mut value = Self {
            bytes: Vec::new(),
            limit,
        };
        value.field(domain)?;
        Ok(value)
    }
    pub fn field(&mut self, bytes: &[u8]) -> Result<(), CaptureError> {
        let added = bytes.len().checked_add(8).ok_or(CaptureError::Limit)?;
        if added > self.limit.saturating_sub(self.bytes.len()) {
            return Err(CaptureError::Limit);
        }
        self.bytes
            .extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    pub fn digest(self) -> foundry_contracts::context::ContextDigest {
        foundry_contracts::context::ContextDigest::of(&self.bytes)
    }
}
