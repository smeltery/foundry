use super::{CaptureError, bounds::Encoding};
use foundry_contracts::{Lifecycle, Outcome, context::ContextDigest};

pub const MAX_TERMINAL_RESULT_BYTES: usize = 16 * 1024;

/// Exact owner-visible proposal; private bytes are neither Debug nor ordinary events.
pub struct CaptureTerminal {
    lifecycle: Lifecycle,
    outcome: Outcome,
    result: String,
    digest: ContextDigest,
}
impl CaptureTerminal {
    pub fn new(lifecycle: Lifecycle, outcome: Outcome, result: &str) -> Result<Self, CaptureError> {
        if lifecycle != Lifecycle::Exit {
            return Err(CaptureError::Invalid);
        }
        if result.len() > MAX_TERMINAL_RESULT_BYTES {
            return Err(CaptureError::Limit);
        }
        let mut encoded = Encoding::new(
            b"foundry.capture.terminal.v1",
            MAX_TERMINAL_RESULT_BYTES + 256,
        )?;
        encoded.field(b"exit")?;
        encoded.field(match outcome {
            Outcome::Success => b"success",
            Outcome::Failure => b"failure",
            Outcome::ActionRequired => b"action_required",
        })?;
        encoded.field(result.as_bytes())?;
        Ok(Self {
            lifecycle,
            outcome,
            result: result.into(),
            digest: encoded.digest(),
        })
    }
    pub fn lifecycle(&self) -> Lifecycle {
        self.lifecycle
    }
    pub fn outcome(&self) -> Outcome {
        self.outcome
    }
    pub fn result(&self) -> &str {
        &self.result
    }
    pub fn binding_digest(&self) -> ContextDigest {
        self.digest
    }
    pub fn into_parts(self) -> (Lifecycle, Outcome, String) {
        (self.lifecycle, self.outcome, self.result)
    }
}
