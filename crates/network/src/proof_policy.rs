use super::ProofError;
use data_encoding::BASE64URL_NOPAD;
use ring::signature::RsaPublicKeyComponents;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Exact issuer, audience and token type; no deployment defaults are supplied.
pub struct ProofConfig {
    pub issuer: String,
    pub audience: String,
    pub token_type: String,
}
pub(super) type Keys = BTreeMap<String, RsaPublicKeyComponents<Vec<u8>>>;

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}
#[derive(Deserialize)]
struct Jwk {
    kty: String,
    kid: String,
    n: String,
    e: String,
    #[serde(default, deserialize_with = "present")]
    alg: Option<String>,
    #[serde(rename = "use", default, deserialize_with = "present")]
    usage: Option<String>,
    #[serde(default, deserialize_with = "present")]
    key_ops: Option<Vec<String>>,
    // Recognize private material explicitly so it cannot become ignored metadata.
    #[serde(rename = "d", default, deserialize_with = "reject_private")]
    _d: (),
    #[serde(rename = "p", default, deserialize_with = "reject_private")]
    _p: (),
    #[serde(rename = "q", default, deserialize_with = "reject_private")]
    _q: (),
    #[serde(rename = "dp", default, deserialize_with = "reject_private")]
    _dp: (),
    #[serde(rename = "dq", default, deserialize_with = "reject_private")]
    _dq: (),
    #[serde(rename = "qi", default, deserialize_with = "reject_private")]
    _qi: (),
    #[serde(rename = "oth", default, deserialize_with = "reject_private")]
    _oth: (),
}

pub(super) fn load(config: &ProofConfig, bytes: &[u8]) -> Result<Keys, ProofError> {
    let bad = || ProofError::InvalidConfig;
    if [&config.issuer, &config.audience, &config.token_type]
        .iter()
        .any(|value| value.is_empty() || value.len() > 2048)
        || bytes.len() > 256 * 1024
    {
        return Err(bad());
    }
    let jwks: Jwks = serde_json::from_slice(bytes).map_err(|_| bad())?;
    if jwks.keys.is_empty() || jwks.keys.len() > 64 {
        return Err(bad());
    }
    let mut keys = Keys::new();
    for jwk in jwks.keys {
        if jwk.kty != "RSA"
            || jwk.kid.is_empty()
            || jwk.kid.len() > 256
            || jwk.alg.as_deref().is_some_and(|value| value != "RS256")
            || jwk.usage.as_deref().is_some_and(|value| value != "sig")
            || jwk.key_ops.as_ref().is_some_and(|ops| ops != &["verify"])
        {
            return Err(bad());
        }
        let n = integer(&jwk.n)?;
        let e = integer(&jwk.e)?;
        let bits = n.len() * 8 - n[0].leading_zeros() as usize;
        if !(2048..=8192).contains(&bits)
            || keys
                .insert(jwk.kid, RsaPublicKeyComponents { n, e })
                .is_some()
        {
            return Err(bad());
        }
    }
    Ok(keys)
}
fn integer(value: &str) -> Result<Vec<u8>, ProofError> {
    let bytes = BASE64URL_NOPAD
        .decode(value.trim_end_matches('=').as_bytes())
        .map_err(|_| ProofError::InvalidConfig)?;
    let first = bytes
        .iter()
        .position(|byte| *byte != 0)
        .ok_or(ProofError::InvalidConfig)?;
    Ok(bytes[first..].to_vec())
}

fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
fn reject_private<'de, D: serde::Deserializer<'de>>(_: D) -> Result<(), D::Error> {
    Err(serde::de::Error::custom(
        "private key parameter is not permitted",
    ))
}
