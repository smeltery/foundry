//! Session create with contract v2 options and the associations read back.
//! Validation and persistence live in the store, so every caller shares them.
use crate::{Result, Service};
use foundry_contracts::{ContractError, CreateSession};
use foundry_store::{CreateReceipt, SessionAssociations, StoreError};
use foundry_tools::ToolProfile;

/// Decodes a create request. A malformed, unknown, incomplete or conflicting
/// field, such as a second checkout selector, is `invalid_request`.
pub fn decode_create_request(json: &str) -> std::result::Result<CreateSession, ContractError> {
    serde_json::from_str(json)
        .map_err(|error| ContractError::Invalid(format!("create request: {error}")))
}

/// Refuses a well-formed tool profile that names no known profile. The
/// contract checks only the format, so a caller that creates through the store
/// directly must run this after `validate` and before the write.
pub fn check_tool_profile_known(request: &CreateSession) -> std::result::Result<(), ContractError> {
    match &request.tool_profile {
        Some(profile) if ToolProfile::named(profile).is_err() => {
            Err(ContractError::Unsupported("tool profile".into()))
        }
        _ => Ok(()),
    }
}

impl Service {
    /// Reports whether the session was newly created or replayed for its key.
    /// An unknown tool profile is unsupported before anything is written.
    pub fn create_session(&mut self, request: CreateSession) -> Result<CreateReceipt> {
        request.validate().map_err(StoreError::from)?;
        check_tool_profile_known(&request).map_err(StoreError::from)?;
        Ok(self.store.create_session(request)?)
    }

    pub fn associations(&self, session_id: &str) -> Result<SessionAssociations> {
        Ok(self.store.associations(session_id)?)
    }
}
