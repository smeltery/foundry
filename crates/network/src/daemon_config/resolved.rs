use super::{ConfigError, NetworkConfig, ResolvedStartup, TransportConfig, TrustConfig, inputs};
use crate::{
    EndpointConfig, WireProfile,
    code_invite::{CodePolicy, Codebook},
    full_team_store::StorePolicy,
    proof::ProofConfig,
};
use iroh::{EndpointAddr, SecretKey};
use std::path::Path;

struct Trust {
    issuer: String,
    audience: String,
    token_type: String,
    jwks: Vec<u8>,
}
impl Trust {
    fn config(&self) -> ProofConfig {
        ProofConfig {
            issuer: self.issuer.clone(),
            audience: self.audience.clone(),
            token_type: self.token_type.clone(),
        }
    }
}
/// Resolved inputs contain bootstrap secrets and deliberately have no Debug.
/// ```compile_fail
/// fn diagnostics<T: std::fmt::Debug>() {}
/// diagnostics::<foundry_network::daemon_config::ResolvedNetwork>();
/// ```
pub struct ResolvedNetwork {
    pub profile: WireProfile,
    pub seeds: Vec<EndpointAddr>,
    pub startup: ResolvedStartup,
    pub expected_team: Option<String>,
    transport: TransportConfig,
    relay: crate::RelaySelection,
    roots: Vec<Vec<u8>>,
    trust: Option<Trust>,
    book: Option<(Vec<u8>, [u8; 32])>,
    compression: Option<crate::ticket::RelayCompression>,
}
impl NetworkConfig {
    /// Reads only explicitly referenced files, relative to the caller's absolute
    /// base. Does not bind sockets, discover peers, create state or read profiles.
    pub fn resolve(&self, base_directory: &Path) -> Result<ResolvedNetwork, ConfigError> {
        if !base_directory.is_absolute() {
            return Err(ConfigError::Invalid);
        }
        self.validate()?;
        let relay = self.transport.relay_selection()?;
        let roots = self
            .transport
            .relay_root_certificates
            .iter()
            .map(|p| inputs::read(base_directory, p, 64 * 1024))
            .collect::<Result<Vec<_>, _>>()?;
        for root in &roots {
            // Check each explicit root independently: a good root must not hide
            // another unparseable certificate silently discarded by TLS loading.
            iroh::tls::CaTlsConfig::custom_roots([root.clone().into()])
                .client_config(iroh::tls::default_provider())
                .map_err(|_| ConfigError::Invalid)?;
        }
        let trust = match &self.trust {
            TrustConfig::Unverified {} => None,
            TrustConfig::Proof {
                issuer,
                audience,
                token_type,
                jwks_file,
            } => Some(Trust {
                issuer: issuer.clone(),
                audience: audience.clone(),
                token_type: token_type.clone(),
                jwks: inputs::read(base_directory, jwks_file, 256 * 1024)?,
            }),
        };
        let book = self
            .codebook
            .as_ref()
            .map(|b| {
                Ok((
                    inputs::read(base_directory, &b.path, 256 * 1024)?,
                    b.digest()?,
                ))
            })
            .transpose()?;
        let mut result = ResolvedNetwork {
            profile: self.profile.clone(),
            seeds: self
                .seeds
                .iter()
                .map(|s| s.address())
                .collect::<Result<Vec<_>, _>>()?,
            startup: ResolvedStartup::Existing,
            expected_team: self.expected_team.clone(),
            transport: self.transport.clone(),
            relay,
            roots,
            trust,
            book,
            compression: self.compression()?,
        };
        // Validate actual verifier/codebook semantics before returning usable inputs.
        result.store_policy()?;
        result.startup =
            self.startup
                .resolve(self, result.code_policy()?.as_ref(), base_directory)?;
        Ok(result)
    }
}
impl ResolvedNetwork {
    pub fn verify_ownership_proof(
        &self,
        endpoint: &iroh::EndpointId,
        token: &str,
        now_seconds: i64,
    ) -> Result<(), ConfigError> {
        let trust = self.trust.as_ref().ok_or(ConfigError::Invalid)?;
        let verifier = crate::proof::ProofVerifier::from_jwks(trust.config(), &trust.jwks)
            .map_err(|_| ConfigError::Invalid)?;
        verifier
            .verify(
                endpoint,
                token,
                crate::proof::ProofUse::Ownership,
                now_seconds,
            )
            .map(|_| ())
            .map_err(|_| ConfigError::Invalid)
    }
    pub fn ticket_codec(&self) -> Result<crate::ticket::TicketCodec<'_>, ConfigError> {
        crate::ticket::TicketCodec::new(&self.profile, self.compression.as_ref())
            .map_err(|_| ConfigError::Invalid)
    }
    pub fn endpoint_config(&self, key: SecretKey) -> EndpointConfig {
        EndpointConfig {
            profile: self.profile.clone(),
            secret_key: key,
            relay: self.relay.clone(),
            bind_address: self.transport.bind_address,
            direct_paths: self.transport.direct_paths,
            relay_root_certificates: self.roots.clone(),
        }
    }
    pub fn code_policy(&self) -> Result<Option<CodePolicy>, ConfigError> {
        self.book
            .as_ref()
            .map(|(bytes, digest)| {
                Codebook::from_bytes(bytes, *digest)
                    .and_then(|book| CodePolicy::new(book, &self.profile))
                    .map_err(|_| ConfigError::Invalid)
            })
            .transpose()
    }
    pub fn store_policy(&self) -> Result<StorePolicy, ConfigError> {
        let proof = self.trust.as_ref().map(|t| (t.config(), t.jwks.as_slice()));
        let policy =
            StorePolicy::new(self.profile.clone(), proof).map_err(|_| ConfigError::Invalid)?;
        match self.code_policy()? {
            Some(codes) => policy.with_codes(codes).map_err(|_| ConfigError::Invalid),
            None => Ok(policy),
        }
    }
}
