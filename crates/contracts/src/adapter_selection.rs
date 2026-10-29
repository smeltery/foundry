//! Public configuration identity; private credentials and resources are excluded.
use crate::context::ContextDigest;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterSelection {
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub directory: String,
    pub workspace_identity: ContextDigest,
    pub profile: String,
    pub profile_version: u32,
    pub permission_profile: String,
    pub resource_policy: String,
    pub resource_identity: ContextDigest,
    pub tool_catalog: String,
    pub tools: Vec<String>,
    pub policy_identity: ContextDigest,
    pub generation: u64,
    pub max_output_bytes: u64,
    pub max_pending_text_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid adapter selection")]
pub struct SelectionError;

impl AdapterSelection {
    pub fn validate(&self) -> Result<(), SelectionError> {
        let text = |value: &str, limit: usize| {
            !value.trim().is_empty() && value.len() <= limit && !value.contains('\0')
        };
        if !text(&self.provider, 128)
            || !text(&self.model, 256)
            || !text(&self.directory, 4096)
            || !Path::new(&self.directory).is_absolute()
            || !text(&self.profile, 128)
            || self.profile_version == 0
            || !matches!(
                self.effort.as_str(),
                "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
            )
            || !matches!(self.permission_profile.as_str(), "current" | "legacy")
            || self.resource_policy != "explicit_system_v1"
            || self.tool_catalog != "core_workspace_v1"
            || !(1..=9_007_199_254_740_991).contains(&self.generation)
            || !(1..=1_048_576).contains(&self.max_output_bytes)
            || !(1..=1_048_576).contains(&self.max_pending_text_bytes)
        {
            return Err(SelectionError);
        }
        let mut previous = None;
        for tool in &self.tools {
            let index = ["list", "read", "write"]
                .iter()
                .position(|allowed| *allowed == tool)
                .ok_or(SelectionError)?;
            if previous.is_some_and(|previous| previous >= index) {
                return Err(SelectionError);
            }
            previous = Some(index);
        }
        Ok(())
    }

    /// Versioned length-framed identity, independent of JSON object key ordering.
    /// Opaque identities are owner-issued labels, not hashes of secret material.
    pub fn digest(&self) -> Result<ContextDigest, SelectionError> {
        self.validate()?;
        let mut bytes = b"foundry.adapter.selection.v1\0".to_vec();
        fn string(bytes: &mut Vec<u8>, value: &str) {
            bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
            bytes.extend_from_slice(value.as_bytes());
        }
        for value in [&self.provider, &self.model, &self.effort, &self.directory] {
            string(&mut bytes, value);
        }
        bytes.extend_from_slice(self.workspace_identity.as_bytes());
        string(&mut bytes, &self.profile);
        bytes.extend_from_slice(&u64::from(self.profile_version).to_be_bytes());
        string(&mut bytes, &self.permission_profile);
        string(&mut bytes, &self.resource_policy);
        bytes.extend_from_slice(self.resource_identity.as_bytes());
        string(&mut bytes, &self.tool_catalog);
        bytes.extend_from_slice(&(self.tools.len() as u64).to_be_bytes());
        for tool in &self.tools {
            string(&mut bytes, tool);
        }
        bytes.extend_from_slice(self.policy_identity.as_bytes());
        for value in [
            self.generation,
            self.max_output_bytes,
            self.max_pending_text_bytes,
        ] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        Ok(ContextDigest::of(&bytes))
    }
}
