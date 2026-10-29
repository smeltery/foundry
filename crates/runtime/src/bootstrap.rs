//! One-use private adapter preparation. Matching acknowledgements confer no authority.
use crate::{AdapterSpec, RuntimeError};
use foundry_contracts::context::ContextDigest;
use std::sync::{Arc, Mutex};

pub const BOOTSTRAP_VERSION: u32 = 1;
pub const MAX_BOOTSTRAP_BODY_BYTES: usize = 65_536;
pub const MAX_BOOTSTRAP_FRAME_BYTES: usize = 96 * 1024;
pub const MAX_BOOTSTRAP_ACK_BYTES: usize = 1024;

#[derive(Clone, Copy)]
pub struct BootstrapIdentity {
    pub schema_version: u32,
    pub selection_digest: ContextDigest,
}
/// Opaque registered private codec. No raw-content Debug, Clone or serialization.
pub struct PrivateBootstrap {
    pub(crate) identity: BootstrapIdentity,
    pub(crate) body: Vec<u8>,
}
impl PrivateBootstrap {
    pub fn new(identity: BootstrapIdentity, body: Vec<u8>) -> Result<Self, RuntimeError> {
        if identity.schema_version != BOOTSTRAP_VERSION {
            return Err(RuntimeError::Invalid("unsupported bootstrap version"));
        }
        if body.is_empty() || body.len() > MAX_BOOTSTRAP_BODY_BYTES {
            return Err(RuntimeError::Invalid("private bootstrap body limit"));
        }
        Ok(Self { identity, body })
    }
}
#[derive(Clone, Default)]
pub(crate) struct BootstrapSlot(Option<Arc<Mutex<Option<PrivateBootstrap>>>>);
impl BootstrapSlot {
    pub(crate) fn take(&self) -> Result<Option<PrivateBootstrap>, RuntimeError> {
        let Some(slot) = &self.0 else { return Ok(None) };
        slot.lock()
            .map_err(|_| RuntimeError::Invalid("bootstrap ownership unavailable"))?
            .take()
            .map(Some)
            .ok_or(RuntimeError::Invalid("private bootstrap already consumed"))
    }
}
impl std::fmt::Debug for BootstrapSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Required"
        } else {
            "Absent"
        })
    }
}
impl AdapterSpec {
    /// Clones share this single-use slot. Failure never restores it for replay.
    pub fn with_private_bootstrap(mut self, bootstrap: PrivateBootstrap) -> Self {
        self.bootstrap = BootstrapSlot(Some(Arc::new(Mutex::new(Some(bootstrap)))));
        self
    }
}
