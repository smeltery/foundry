//! Explicit local trust policy for endpoint-bound compact attestations.
#[path = "proof_policy.rs"]
mod policy;
use data_encoding::BASE64URL_NOPAD;
use iroh::EndpointId;
pub use policy::ProofConfig;
use ring::signature::RSA_PKCS1_2048_8192_SHA256;
use serde::Deserialize;
use std::fmt;

const MAX_TOKEN_BYTES: usize = 4096;
const FUTURE_ALLOWANCE_SECONDS: i64 = 86_400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofUse {
    Current,
    Ownership,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofError {
    InvalidConfig,
    TooLong,
    Malformed,
    Algorithm,
    TokenType,
    UnknownKey,
    Signature,
    Issuer,
    Audience,
    Expired,
    Future,
    Endpoint,
    Person,
}
impl fmt::Display for ProofError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfig => "invalid proof trust configuration",
            Self::TooLong => "proof exceeds size limit",
            Self::Malformed => "malformed proof",
            Self::Algorithm => "unsupported proof algorithm",
            Self::TokenType => "unexpected proof type",
            Self::UnknownKey => "unknown proof key",
            Self::Signature => "invalid proof signature",
            Self::Issuer => "unexpected proof issuer",
            Self::Audience => "unexpected proof audience",
            Self::Expired => "proof expired",
            Self::Future => "proof issued too far in future",
            Self::Endpoint => "proof endpoint mismatch",
            Self::Person => "invalid proof person name",
        })
    }
}
impl std::error::Error for ProofError {}

/// Caller-selected authority. Construction never reads environment or network.
/// Team operations must use their own configured verifier, not caller claims.
pub struct ProofVerifier {
    config: ProofConfig,
    keys: policy::Keys,
}
impl fmt::Debug for ProofVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProofVerifier")
            .field("key_count", &self.keys.len())
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    kid: String,
    typ: String,
}
#[derive(Deserialize)]
struct Claims {
    iss: String,
    aud: String,
    sub: String,
    name: String,
    email: String,
    endpoint: String,
    iat: i64,
    exp: i64,
}

/// Valid only for the recorded policy, use and clock snapshot. Not a durable
/// authorization object: persist the token privately and verify on each use.
pub struct VerifiedProof<'policy> {
    _verifier: &'policy ProofVerifier,
    claims: Claims,
    endpoint: EndpointId,
    token: String,
    usage: ProofUse,
    checked_at: i64,
}
impl fmt::Debug for VerifiedProof<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VerifiedProof { .. }")
    }
}
impl VerifiedProof<'_> {
    pub fn endpoint(&self) -> EndpointId {
        self.endpoint
    }
    pub fn subject(&self) -> &str {
        &self.claims.sub
    }
    pub fn person(&self) -> &str {
        &self.claims.name
    }
    pub fn email(&self) -> &str {
        &self.claims.email
    }
    pub fn issued_at(&self) -> i64 {
        self.claims.iat
    }
    pub fn expires_at(&self) -> i64 {
        self.claims.exp
    }
    pub fn usage(&self) -> ProofUse {
        self.usage
    }
    pub fn checked_at(&self) -> i64 {
        self.checked_at
    }
    /// Sensitive wire/storage data; never include in ordinary status or logs.
    pub fn token_for_storage(&self) -> &str {
        &self.token
    }
    pub fn replacement_key(&self) -> (i64, &str) {
        (self.claims.iat, &self.token)
    }
}

impl ProofVerifier {
    pub fn from_jwks(config: ProofConfig, jwks: &[u8]) -> Result<Self, ProofError> {
        let keys = policy::load(&config, jwks)?;
        Ok(Self { config, keys })
    }

    pub fn verify(
        &self,
        endpoint: &EndpointId,
        token: &str,
        usage: ProofUse,
        now_seconds: i64,
    ) -> Result<VerifiedProof<'_>, ProofError> {
        if token.len() > MAX_TOKEN_BYTES {
            return Err(ProofError::TooLong);
        }
        let token = token.trim();
        let mut parts = token.split('.');
        let (Some(header), Some(payload), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(ProofError::Malformed);
        };
        let parsed: Header =
            serde_json::from_slice(&decode(header)?).map_err(|_| ProofError::Malformed)?;
        if parsed.alg != "RS256" {
            return Err(ProofError::Algorithm);
        }
        if parsed.typ != self.config.token_type {
            return Err(ProofError::TokenType);
        }
        let key = self.keys.get(&parsed.kid).ok_or(ProofError::UnknownKey)?;
        // Preserve the signed bytes exactly, including JSON whitespace/order.
        let signed = &token.as_bytes()[..header.len() + 1 + payload.len()];
        key.verify(&RSA_PKCS1_2048_8192_SHA256, signed, &decode(signature)?)
            .map_err(|_| ProofError::Signature)?;
        let claims: Claims =
            serde_json::from_slice(&decode(payload)?).map_err(|_| ProofError::Malformed)?;
        if claims.iss != self.config.issuer {
            return Err(ProofError::Issuer);
        }
        if claims.aud != self.config.audience {
            return Err(ProofError::Audience);
        }
        if usage == ProofUse::Current && claims.exp <= now_seconds {
            return Err(ProofError::Expired);
        }
        if claims.iat > now_seconds.saturating_add(FUTURE_ALLOWANCE_SECONDS) {
            return Err(ProofError::Future);
        }
        if claims.endpoint != endpoint.to_string() {
            return Err(ProofError::Endpoint);
        }
        if claims.name.is_empty()
            || claims.name.len() > 40
            || !claims
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(ProofError::Person);
        }
        Ok(VerifiedProof {
            _verifier: self,
            claims,
            endpoint: *endpoint,
            token: token.into(),
            usage,
            checked_at: now_seconds,
        })
    }
}
fn decode(value: &str) -> Result<Vec<u8>, ProofError> {
    BASE64URL_NOPAD
        .decode(value.as_bytes())
        .map_err(|_| ProofError::Malformed)
}
