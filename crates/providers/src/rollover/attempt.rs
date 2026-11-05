//! The attempt a rollover decision is about and the failure that ended it.
use super::RolloverError;

/// Names one attempt: 1 to 256 bytes without NUL, the bound the usage meter
/// applies to attempt IDs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AttemptId(String);

impl AttemptId {
    pub const MAX_BYTES: usize = 256;

    pub fn new(value: &str) -> Result<Self, RolloverError> {
        if value.is_empty() || value.len() > Self::MAX_BYTES || value.contains('\0') {
            return Err(RolloverError::InvalidAttemptId);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The neutral error class an attempt failed with. Only `quota_exhausted` and
/// `rate_limited` are provider account limits; `budget_exhausted` is a local
/// ceiling that another account would not lift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptFailure {
    QuotaExhausted,
    RateLimited { retry_after_secs: u64 },
    BudgetExhausted,
    InvalidRequest,
    Unauthenticated,
    Forbidden,
    NotFound,
    ModelUnavailable,
    EndpointFailure,
    Unavailable,
}

impl AttemptFailure {
    pub fn class(self) -> &'static str {
        match self {
            Self::QuotaExhausted => "quota_exhausted",
            Self::RateLimited { .. } => "rate_limited",
            Self::BudgetExhausted => "budget_exhausted",
            Self::InvalidRequest => "invalid_request",
            Self::Unauthenticated => "unauthenticated",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::ModelUnavailable => "model_unavailable",
            Self::EndpointFailure => "endpoint_failure",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Where the session's current attempt stands. A running attempt keeps its
/// account to the end; only an ended attempt can lead to a rollover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptState {
    Running {
        attempt_id: AttemptId,
    },
    Ended {
        attempt_id: AttemptId,
        failure: AttemptFailure,
    },
}

impl AttemptState {
    pub fn attempt_id(&self) -> &AttemptId {
        match self {
            Self::Running { attempt_id } | Self::Ended { attempt_id, .. } => attempt_id,
        }
    }
}
