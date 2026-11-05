//! Who and what a metered reservation charges for. The cost and the settlement
//! time are never supplied by the caller: they come from the settlement itself.
use crate::error::BudgetError;

/// Longest attempt, session or user identifier, in bytes. Provisional.
pub const ID_MAX_BYTES: usize = 256;
const PROVIDER_ID_MAX_CHARS: usize = 64;
const ACCOUNT_KEY_MAX_CHARS: usize = 512;
const MODEL_MAX_BYTES: usize = 256;

/// The attempt a metered reservation belongs to. Its settlement writes one
/// usage row in the same transaction that commits the ledger, so reports and
/// `committed` spend always agree. An attempt with several model calls
/// reserves once per call; its rows share `attempt_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    pub attempt_id: String,
    pub provider_id: String,
    pub account_key: String,
    pub model: String,
    /// `None` reports as `unattributed` when grouping by session.
    pub session_id: Option<String>,
    /// `None` reports as `unattributed` when grouping by user.
    pub user_id: Option<String>,
}

impl Attempt {
    /// Checks every field against the provider limits before anything is reserved.
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
