use super::{Error, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use foundry_contracts::context::ContextDigest;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Blob {
    bytes: u32,
    sha256: ContextDigest,
    base64: String,
}
impl Blob {
    pub fn new(bytes: &[u8], limit: usize) -> Result<Self> {
        if bytes.len() > limit || bytes.len() > 1048576 {
            return Err(Error::Invalid);
        }
        Ok(Self {
            bytes: bytes.len() as u32,
            sha256: ContextDigest::of(bytes),
            base64: STANDARD.encode(bytes),
        })
    }
    pub fn decode(&self, limit: usize) -> Result<Vec<u8>> {
        if self.bytes as usize > limit || self.base64.len() > limit.div_ceil(3) * 4 {
            return Err(Error::Invalid);
        }
        let bytes = STANDARD.decode(&self.base64).map_err(|_| Error::Invalid)?;
        if bytes.len() != self.bytes as usize
            || ContextDigest::of(&bytes) != self.sha256
            || STANDARD.encode(&bytes) != self.base64
        {
            return Err(Error::Invalid);
        }
        Ok(bytes)
    }
}
