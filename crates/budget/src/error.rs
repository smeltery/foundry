//! Neutral error classes from the provider and budget specification.
//! Class and cause strings are provisional wire names.
use crate::store::BudgetId;
use std::fmt;

/// Why a reservation was refused with `budget_exhausted`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExhaustedCause {
    /// Committed spend plus open reservations plus the request exceeds a ceiling.
    Ceiling,
    /// An earlier settlement exceeded its reservation; further dispatch is refused.
    Overrun,
    /// The budget's wall-clock deadline has passed.
    DeadlineExceeded,
}

impl ExhaustedCause {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ceiling => "ceiling",
            Self::Overrun => "overrun",
            Self::DeadlineExceeded => "deadline_exceeded",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BudgetError {
    /// A malformed or out-of-range value; `field` names the offending input.
    InvalidRequest {
        field: &'static str,
    },
    /// A local ceiling, overrun or deadline refuses the work. `budget` is the
    /// budget in the delegation chain that refused, which may be an ancestor.
    BudgetExhausted {
        budget: BudgetId,
        cause: ExhaustedCause,
    },
    NotFound {
        what: &'static str,
    },
    /// The operation does not apply to the current state, such as settling a
    /// reservation twice or recording a review round after the review ended.
    Conflict {
        what: &'static str,
    },
    /// The store is unusable because an earlier operation panicked mid-update.
    Unavailable,
}

impl BudgetError {
    pub(crate) fn invalid(field: &'static str) -> Self {
        Self::InvalidRequest { field }
    }

    /// The neutral error class string.
    pub fn class(&self) -> &'static str {
        match self {
            Self::InvalidRequest { .. } => "invalid_request",
            Self::BudgetExhausted { .. } => "budget_exhausted",
            Self::NotFound { .. } => "not_found",
            Self::Conflict { .. } => "conflict",
            Self::Unavailable => "unavailable",
        }
    }
}

impl fmt::Display for BudgetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest { field } => write!(formatter, "invalid_request: {field}"),
            Self::BudgetExhausted { budget, cause } => write!(
                formatter,
                "budget_exhausted: budget {} ({})",
                budget.get(),
                cause.name()
            ),
            Self::NotFound { what } => write!(formatter, "not_found: {what}"),
            Self::Conflict { what } => write!(formatter, "conflict: {what}"),
            Self::Unavailable => write!(formatter, "unavailable: budget store"),
        }
    }
}

impl std::error::Error for BudgetError {}
