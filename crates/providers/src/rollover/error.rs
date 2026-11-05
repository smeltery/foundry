use std::fmt;

/// A rollover request that is inconsistent with itself. Diagnostics never echo
/// identifiers, so a caller can log them as they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RolloverError {
    InvalidAttemptId,
    /// The proposed new attempt reuses the current attempt or one already
    /// linked by a record, so the link would not name a new attempt.
    AttemptReused,
    /// The latest record does not lead to the current attempt and account, or
    /// a record names another workspace or model, so the records are not this
    /// attempt's chain.
    HistoryMismatch,
    /// Consecutive records do not link account to account and attempt to
    /// attempt, or the chain names an attempt twice.
    BrokenChain,
    /// The same account of the session's workspace and provider was offered
    /// twice.
    DuplicateCandidate,
}

impl RolloverError {
    /// Provisional neutral error class from the providers specification.
    pub fn class(self) -> &'static str {
        "invalid_request"
    }
}

impl fmt::Display for RolloverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "account rollover request rejected ({self:?})")
    }
}

impl std::error::Error for RolloverError {}
