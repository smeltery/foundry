//! Proof key for code exchange, `S256` only. The `plain` method, and an absent
//! method that would default to it, are refused: a plain challenge is the
//! verifier itself, so it protects nothing once the request is observed.
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use super::error::OAuthError;

/// The only accepted challenge method name; provisional wire name.
pub const S256: &str = "S256";

/// Verifier length bounds from the PKCE grammar.
const VERIFIER_LENGTH: std::ops::RangeInclusive<usize> = 43..=128;
/// A base64url SHA-256 digest without padding.
const CHALLENGE_LENGTH: usize = 43;

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// Checks the challenge and its method as sent with the authorization request.
pub(super) fn challenge(
    challenge: Option<&str>,
    method: Option<&str>,
) -> Result<String, OAuthError> {
    if method != Some(S256) {
        return Err(OAuthError::InvalidRequest {
            field: "code_challenge_method",
        });
    }
    let challenge = challenge.ok_or(OAuthError::InvalidRequest {
        field: "code_challenge",
    })?;
    let well_formed = challenge.len() == CHALLENGE_LENGTH
        && challenge
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if !well_formed {
        return Err(OAuthError::InvalidRequest {
            field: "code_challenge",
        });
    }
    Ok(challenge.to_owned())
}

/// The `S256` challenge for `verifier`, or `None` when the verifier is outside
/// the grammar and so can match no challenge.
pub(super) fn transform(verifier: &str) -> Option<String> {
    let well_formed =
        VERIFIER_LENGTH.contains(&verifier.len()) && verifier.bytes().all(is_unreserved);
    well_formed.then(|| URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())))
}
