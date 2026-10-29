//! Private capture data and trusted-owner attestations, never native semantic proof.
mod bounds;
mod cut;
mod restored;
mod terminal;
mod trace;
mod validation;
pub use cut::*;
pub use restored::RestoredCaptureOrigin;
pub use terminal::*;
pub use trace::*;
pub use validation::*;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("capture data exceeds its limit")]
    Limit,
    #[error("invalid capture identity or metadata")]
    Invalid,
    #[error("capture binding mismatch")]
    Binding,
    #[error("capture delivery state is unresolved")]
    Pending,
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod terminal_tests;
