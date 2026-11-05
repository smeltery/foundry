//! Bearer extraction from the authorization header. Token bytes stay inside
//! `BearerToken`: no error, `Debug` output or route decision carries them.

use std::fmt;

/// A syntactically valid bearer credential. It has no `Display`, and its
/// `Debug` output is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct BearerToken(String);

impl BearerToken {
    /// The credential bytes, for the resolver alone. Never log or echo them.
    pub fn secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BearerToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerToken(<redacted>)")
    }
}

/// Why no bearer could be extracted. Every variant is `unauthenticated`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BearerError {
    Missing,
    /// More than one authorization header; none is trusted.
    Repeated,
    /// A scheme other than `Bearer`.
    WrongScheme,
    /// A bearer scheme whose credential is empty or not `token68`.
    Malformed,
}

/// Extracts the bearer from every value of the authorization header.
pub fn extract_bearer(values: &[&str]) -> Result<BearerToken, BearerError> {
    let value = match values {
        [] => return Err(BearerError::Missing),
        [value] => value.trim_matches([' ', '\t']),
        _ => return Err(BearerError::Repeated),
    };
    let (scheme, credential) = value.split_once(' ').unwrap_or((value, ""));
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(BearerError::WrongScheme);
    }
    let credential = credential.trim_start_matches(' ');
    if !is_token68(credential) {
        return Err(BearerError::Malformed);
    }
    Ok(BearerToken(credential.to_owned()))
}

/// `token68`: one or more of letters, digits and `-._~+/`, then optional `=`
/// padding.
fn is_token68(credential: &str) -> bool {
    let body = credential.trim_end_matches('=');
    !body.is_empty()
        && body
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~+/".contains(&byte))
}
