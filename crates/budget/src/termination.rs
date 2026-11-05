//! Settlement values the budget core hands to the runtime when it ends an attempt.
//! They follow the completion policy: `suspended` carries no outcome, and `exit`
//! always does. Names are provisional pending NIC-34 D1/D6.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    Exit,
    Suspended,
}

impl Lifecycle {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::Suspended => "suspended",
        }
    }
}

/// The only outcome the budget core ever decides. Success is never decided here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Failure,
}

impl Outcome {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Failure => "failure",
        }
    }
}

/// Which provider-side limit refused the work. Codes are provisional; hosted plan
/// and billing semantics are held on D46-5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuotaCause {
    AccountLimit,
    PlanLimit,
    Billing,
}

impl QuotaCause {
    pub fn name(&self) -> &'static str {
        match self {
            Self::AccountLimit => "account_limit",
            Self::PlanLimit => "plan_limit",
            Self::Billing => "billing",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    DeadlineExceeded,
    QuotaExhausted(QuotaCause),
    NeedsReview,
}

impl Cause {
    pub fn name(&self) -> &'static str {
        match self {
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::QuotaExhausted(_) => "quota_exhausted",
            Self::NeedsReview => "needs_review",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Termination {
    pub lifecycle: Lifecycle,
    pub outcome: Option<Outcome>,
    pub cause: Cause,
}

impl Termination {
    /// A passed deadline cancels the running attempt gracefully; it never succeeds.
    pub fn deadline_exceeded() -> Self {
        Self {
            lifecycle: Lifecycle::Suspended,
            outcome: None,
            cause: Cause::DeadlineExceeded,
        }
    }

    /// A provider account, plan or billing refusal suspends the attempt so it can
    /// resume once the limit lifts. The caller sees the `quota_exhausted` class.
    pub fn quota_exhausted(cause: QuotaCause) -> Self {
        Self {
            lifecycle: Lifecycle::Suspended,
            outcome: None,
            cause: Cause::QuotaExhausted(cause),
        }
    }

    /// The reviewer could not verify the work within the allowed rounds.
    pub fn needs_review() -> Self {
        Self {
            lifecycle: Lifecycle::Exit,
            outcome: Some(Outcome::Failure),
            cause: Cause::NeedsReview,
        }
    }
}
