use crate::{NetworkError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy)]
pub enum Channel {
    Team,
    Join,
}

/// Deployment-supplied protocol identifiers. No legacy or default values exist.
/// Validation is structural, not a claim of compatibility or trust.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireProfile {
    pub team_alpn: Vec<u8>,
    pub join_alpn: Vec<u8>,
    pub protocol: u32,
    pub signing_prefix: String,
    pub invitation_endpoint_context: String,
    pub invitation_secret_context: String,
    pub ticket_prefixes: Vec<String>,
}
impl WireProfile {
    pub fn validate(&self) -> Result<()> {
        for alpn in [&self.team_alpn, &self.join_alpn] {
            if alpn.is_empty() || alpn.len() > 255 {
                return Err(NetworkError::Invalid("ALPN must contain 1 to 255 bytes"));
            }
        }
        if self.team_alpn == self.join_alpn || self.protocol == 0 {
            return Err(NetworkError::Invalid(
                "distinct channels and positive protocol required",
            ));
        }
        for text in [
            &self.signing_prefix,
            &self.invitation_endpoint_context,
            &self.invitation_secret_context,
        ] {
            if text.trim().is_empty() || text.len() > 256 || text.chars().any(char::is_control) {
                return Err(NetworkError::Invalid(
                    "profile context must be bounded printable text",
                ));
            }
        }
        if self.invitation_endpoint_context == self.invitation_secret_context {
            return Err(NetworkError::Invalid(
                "invitation contexts must be distinct",
            ));
        }
        if self.ticket_prefixes.is_empty() || self.ticket_prefixes.len() > 8 {
            return Err(NetworkError::Invalid(
                "one to eight ticket prefixes required",
            ));
        }
        for (index, prefix) in self.ticket_prefixes.iter().enumerate() {
            if prefix.is_empty()
                || prefix.len() > 64
                || !prefix
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
                || self.ticket_prefixes[..index].contains(prefix)
            {
                return Err(NetworkError::Invalid(
                    "ticket prefixes must be distinct bounded tokens",
                ));
            }
        }
        Ok(())
    }
    pub fn alpn(&self, channel: Channel) -> &[u8] {
        match channel {
            Channel::Team => &self.team_alpn,
            Channel::Join => &self.join_alpn,
        }
    }
}
