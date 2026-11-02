use super::{ConfigError, NetworkConfig, SecretText};
use crate::{
    code_invite::{CanonicalCode, CodePolicy},
    ticket::{Ticket, TicketCodec},
};
use iroh::EndpointId;
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum StartupMode {
    Existing {},
    Form {
        name: String,
        relay: Option<String>,
        proof: Option<SecretText>,
    },
    JoinTicket {
        ticket: SecretText,
        expected_founder: Option<EndpointId>,
        proof: Option<SecretText>,
        #[serde(default)]
        owner_only: bool,
    },
    JoinCode {
        code: SecretText,
        expected_team: Option<String>,
        expected_founder: Option<EndpointId>,
        proof: Option<SecretText>,
        #[serde(default)]
        owner_only: bool,
    },
}
/// Secret-bearing bootstrap inputs. Authenticate the actual connection before use.
/// ```compile_fail
/// fn diagnostics<T: std::fmt::Debug>() {}
/// diagnostics::<foundry_network::daemon_config::ResolvedStartup>();
/// ```
pub enum ResolvedStartup {
    Existing,
    Form {
        name: String,
        relay: Option<String>,
        proof: Option<String>,
    },
    JoinTicket {
        ticket: Ticket,
        expected_founder: Option<EndpointId>,
        proof: Option<String>,
        owner_only: bool,
    },
    JoinCode {
        code: CanonicalCode,
        expected_team: Option<String>,
        expected_founder: Option<EndpointId>,
        proof: Option<String>,
        owner_only: bool,
    },
}
impl StartupMode {
    pub(super) fn validate(&self, config: &NetworkConfig) -> Result<(), ConfigError> {
        let proof = match self {
            Self::Existing {} => None,
            Self::Form { name, relay, proof } => {
                if name.trim().is_empty() {
                    return Err(ConfigError::Invalid);
                }
                if let Some(url) = relay {
                    super::network::relay(url)?;
                }
                proof.as_ref()
            }
            Self::JoinTicket { ticket, proof, .. } => {
                ticket.validate(256 * 1024)?;
                if let Some(text) = ticket.inline() {
                    let compression = config.compression()?;
                    TicketCodec::new(&config.profile, compression.as_ref())
                        .and_then(|codec| codec.decode(text))
                        .map_err(|_| ConfigError::Invalid)?;
                }
                proof.as_ref()
            }
            Self::JoinCode {
                code,
                expected_team,
                proof,
                ..
            } => {
                code.validate(4096)?;
                if config.codebook.is_none()
                    || expected_team
                        .as_ref()
                        .is_some_and(|t| t.is_empty() || t.len() > 256)
                {
                    return Err(ConfigError::Invalid);
                }
                proof.as_ref()
            }
        };
        if let Some(proof) = proof {
            proof.validate(256 * 1024)?;
        }
        Ok(())
    }
    pub(super) fn resolve(
        &self,
        config: &NetworkConfig,
        codes: Option<&CodePolicy>,
        base: &Path,
    ) -> Result<ResolvedStartup, ConfigError> {
        let proof = |value: &Option<SecretText>| {
            value
                .as_ref()
                .map(|v| v.resolve(base, 256 * 1024))
                .transpose()
        };
        Ok(match self {
            Self::Existing {} => ResolvedStartup::Existing,
            Self::Form {
                name,
                relay,
                proof: input,
            } => ResolvedStartup::Form {
                name: name.clone(),
                relay: relay.clone(),
                proof: proof(input)?,
            },
            Self::JoinTicket {
                ticket,
                expected_founder,
                proof: input,
                owner_only,
            } => {
                let compression = config.compression()?;
                let text = ticket.resolve(base, 256 * 1024)?;
                let ticket = TicketCodec::new(&config.profile, compression.as_ref())
                    .and_then(|codec| codec.decode(&text))
                    .map_err(|_| ConfigError::Invalid)?;
                ResolvedStartup::JoinTicket {
                    ticket,
                    expected_founder: *expected_founder,
                    proof: proof(input)?,
                    owner_only: *owner_only,
                }
            }
            Self::JoinCode {
                code,
                expected_team,
                expected_founder,
                proof: input,
                owner_only,
            } => {
                let code = codes
                    .ok_or(ConfigError::Invalid)?
                    .normalize(&code.resolve(base, 4096)?)
                    .map_err(|_| ConfigError::Invalid)?;
                ResolvedStartup::JoinCode {
                    code,
                    expected_team: expected_team.clone(),
                    expected_founder: *expected_founder,
                    proof: proof(input)?,
                    owner_only: *owner_only,
                }
            }
        })
    }
}
