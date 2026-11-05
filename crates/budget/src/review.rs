//! Review rounds for long-running work. `rounds_max` counts the first pass.
use crate::error::BudgetError;
use crate::termination::Termination;

pub const DEFAULT_REVIEW_ROUNDS: u32 = 4;
pub const MAX_REVIEW_ROUNDS: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReviewPolicy {
    rounds_max: u32,
}

impl ReviewPolicy {
    /// Long-running work gets `rounds_max` rounds (default 4, range 1 to 16).
    /// Work that is not long-running, and every follow-up message, runs a single
    /// pass. An out-of-range `rounds_max` is refused even when it would be unused.
    pub fn new(
        long_running: bool,
        rounds_max: Option<u32>,
        follow_up: bool,
    ) -> Result<Self, BudgetError> {
        if rounds_max.is_some_and(|rounds| !(1..=MAX_REVIEW_ROUNDS).contains(&rounds)) {
            return Err(BudgetError::invalid("rounds_max"));
        }
        let rounds_max = if long_running && !follow_up {
            rounds_max.unwrap_or(DEFAULT_REVIEW_ROUNDS)
        } else {
            1
        };
        Ok(Self { rounds_max })
    }

    pub fn rounds_max(&self) -> u32 {
        self.rounds_max
    }

    pub fn start(&self) -> ReviewRounds {
        ReviewRounds {
            rounds_max: self.rounds_max,
            completed: 0,
            ended: false,
        }
    }
}

/// What follows a round the reviewer has checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewStep {
    /// The reviewer verified the derived acceptance criteria. The final outcome
    /// is still decided by the completion policy, not here.
    Verified,
    /// Another round examines and continues the previous work.
    Continue { next_round: u32 },
    /// Rounds are spent and the work is unverified.
    Ended(Termination),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReviewRounds {
    rounds_max: u32,
    completed: u32,
    ended: bool,
}

impl ReviewRounds {
    pub fn completed(&self) -> u32 {
        self.completed
    }

    /// Records the reviewer's verdict on the round that just finished.
    pub fn record(&mut self, criteria_verified: bool) -> Result<ReviewStep, BudgetError> {
        if self.ended {
            return Err(BudgetError::Conflict {
                what: "review_round",
            });
        }
        self.completed += 1;
        let step = if criteria_verified {
            ReviewStep::Verified
        } else if self.completed < self.rounds_max {
            ReviewStep::Continue {
                next_round: self.completed + 1,
            }
        } else {
            ReviewStep::Ended(Termination::needs_review())
        };
        self.ended = !matches!(step, ReviewStep::Continue { .. });
        Ok(step)
    }
}
