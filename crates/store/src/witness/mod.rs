//! Bounded storage witness codec. Structural integrity is not native semantic proof.
mod encoding;
mod preflight;
mod types;
mod validate;
mod wire;
use crate::{Result, checkpoint_codec::invalid};
use std::io::{self, Write};
pub use types::*;
use wire::Wire;

pub const MAX_WITNESS_LOGICAL_RECORDS: usize = 1024;
pub const MAX_WITNESS_PREFIX_RECORDS: usize = 4096;
pub const MAX_WITNESS_MAPPING_REFERENCES: usize = 4096;
pub const MAX_WITNESS_BYTES: usize = 256 * 1024;
pub const MAX_WITNESS_DEPTH: usize = 16;
pub const MAX_WITNESS_SELECTED_EVENT_ROWS: usize = 1024;

/// Private canonical bytes, not a checkpoint association or native approval.
pub struct EncodedWitness {
    bytes: Vec<u8>,
    digest: [u8; 32],
}
impl EncodedWitness {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}
impl WitnessBodyV1 {
    pub fn encode(&self) -> Result<EncodedWitness> {
        validate::structure(self)?;
        // First pass validates every identifier/integer and counts exact JSON escaping
        // before allocating output or cloning caller-owned fields.
        let mut measure = Measure(0);
        self.write(&mut measure)?;
        let mut bytes = Vec::with_capacity(measure.0);
        self.write(&mut bytes)?;
        let digest = crate::checkpoint_cut::digest(b"foundry.checkpoint.witness.v1", &[&bytes]);
        Ok(EncodedWitness { bytes, digest })
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        preflight::check(bytes)?;
        let body = Self::read(serde_json::from_slice(bytes)?)?;
        let canonical = body.encode()?;
        if canonical.bytes() != bytes {
            return Err(invalid("noncanonical witness encoding"));
        }
        Ok(body)
    }
}
struct Measure(usize);
impl Write for Measure {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_WITNESS_BYTES)
            .ok_or_else(|| io::Error::other("witness encoded byte limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[cfg(test)]
mod tests;
