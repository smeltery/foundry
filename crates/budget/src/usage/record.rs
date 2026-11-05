//! One attempt's metered usage: its token and compute counts and its cost.
use crate::amount::{Amount, CostUnit};
use crate::error::BudgetError;
use crate::price::Usage;

/// Longest attempt, session or user identifier, in bytes. Provisional.
pub const ID_MAX_BYTES: usize = 256;
const PROVIDER_ID_MAX_CHARS: usize = 64;
const ACCOUNT_KEY_MAX_CHARS: usize = 512;
const MODEL_MAX_BYTES: usize = 256;

/// The usage and cost of one attempt, recorded once when the attempt settles.
/// `attempt_id` identifies the record: recording the same attempt again is a
/// no-op, so a retried settlement is never counted twice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptUsage {
    pub attempt_id: String,
    pub provider_id: String,
    pub account_key: String,
    pub model: String,
    /// `None` reports as `unattributed` when grouping by session.
    pub session_id: Option<String>,
    /// `None` reports as `unattributed` when grouping by user.
    pub user_id: Option<String>,
    pub usage: Usage,
    /// The attempt's money cost, for example from [`crate::PriceTable::cost`].
    pub cost: Amount,
}

/// What [`crate::usage::UsageStore::record`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recording {
    /// The attempt was new and is now counted.
    Recorded,
    /// An identical record for the attempt already existed; nothing changed.
    AlreadyRecorded,
}

impl AttemptUsage {
    /// Checks every field against the provider limits before anything is stored.
    pub(crate) fn validate(&self) -> Result<(), BudgetError> {
        check_id(&self.attempt_id, "attempt_id")?;
        check_chars(&self.provider_id, PROVIDER_ID_MAX_CHARS, "provider_id")?;
        check_chars(&self.account_key, ACCOUNT_KEY_MAX_CHARS, "account_key")?;
        if self.model.is_empty() || self.model.len() > MODEL_MAX_BYTES || self.model.contains('\0')
        {
            return Err(BudgetError::invalid("model"));
        }
        if let Some(session_id) = &self.session_id {
            check_id(session_id, "session_id")?;
        }
        if let Some(user_id) = &self.user_id {
            check_id(user_id, "user_id")?;
        }
        if !matches!(self.cost.unit(), CostUnit::Money(_)) {
            return Err(BudgetError::invalid("cost"));
        }
        Ok(())
    }
}

fn check_id(value: &str, field: &'static str) -> Result<(), BudgetError> {
    if value.is_empty() || value.len() > ID_MAX_BYTES || value.contains('\0') {
        return Err(BudgetError::invalid(field));
    }
    Ok(())
}

fn check_chars(value: &str, max: usize, field: &'static str) -> Result<(), BudgetError> {
    let count = value.chars().count();
    if count == 0 || count > max {
        return Err(BudgetError::invalid(field));
    }
    Ok(())
}
