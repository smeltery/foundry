//! The authorization code grant with mandatory `S256` PKCE. A code is consumed
//! by the first presentation that carries its secret, whatever the outcome,
//! so a failed verifier guess cannot be retried. Presenting a consumed code
//! again is a replay: it fails and revokes the tokens that code issued.
use std::collections::BTreeSet;

use super::error::OAuthError;
use super::token::{Grant, TokenGrant};
use super::{AuthorizationServer, ClientAuth, LoopbackUri, deadline, pkce};
use crate::store::{SecretText, secret};

/// Binds the stored challenge digest to a fixed label rather than a prefix.
const CHALLENGE_LABEL: &str = "pkce.s256";

pub(super) struct CodeGrant {
    client: String,
    redirect_uri: LoopbackUri,
    subject: String,
    scopes: BTreeSet<String>,
    /// Digest of the `S256` challenge, compared in constant time.
    challenge_digest: [u8; 32],
    expires_ms: u64,
    consumed: bool,
    /// The family of the tokens this code issued.
    family: Option<u64>,
}

/// An authorization request after the user approved it. Every field is as the
/// client sent it; `state` is returned unchanged.
#[derive(Clone, Copy)]
pub struct AuthorizationRequest<'a> {
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub scope: &'a str,
    pub state: Option<&'a str>,
    pub code_challenge: Option<&'a str>,
    pub code_challenge_method: Option<&'a str>,
}

/// The redirect back to the client.
#[derive(Debug, Clone)]
pub struct AuthorizationCode {
    pub code: SecretText,
    pub redirect_uri: LoopbackUri,
    pub state: Option<String>,
}

/// The token request for a code. It has no `Debug`, so the code and verifier
/// cannot reach a log through it.
#[derive(Clone, Copy)]
pub struct CodeExchange<'a> {
    pub code: &'a str,
    pub redirect_uri: &'a str,
    pub code_verifier: &'a str,
}

impl AuthorizationServer {
    /// Issues a code for `subject`, who approved `request`. The redirect URI
    /// must equal one registered for the client byte for byte.
    pub fn authorize(
        &self,
        request: &AuthorizationRequest<'_>,
        subject: &str,
    ) -> Result<AuthorizationCode, OAuthError> {
        if subject.is_empty() {
            return Err(OAuthError::InvalidRequest { field: "subject" });
        }
        let lifetime = self.lifetimes.code;
        self.with_state(|state, now_ms| {
            let client = state
                .clients
                .get(request.client_id)
                .ok_or(OAuthError::InvalidClient)?;
            let redirect_uri = client
                .redirect_uris
                .iter()
                .find(|uri| uri.as_str() == request.redirect_uri)
                .cloned()
                .ok_or(OAuthError::InvalidRequest {
                    field: "redirect_uri",
                })?;
            let challenge = pkce::challenge(request.code_challenge, request.code_challenge_method)?;
            let scopes = client.grantable(request.scope)?;
            let grant = CodeGrant {
                client: client.id.clone(),
                redirect_uri: redirect_uri.clone(),
                subject: subject.to_owned(),
                scopes,
                challenge_digest: secret::digest(CHALLENGE_LABEL, &challenge),
                expires_ms: deadline(now_ms, lifetime),
                consumed: false,
                family: None,
            };
            let (_, code) = state.codes.issue(grant)?;
            Ok(AuthorizationCode {
                code,
                redirect_uri,
                state: request.state.map(str::to_owned),
            })
        })
    }

    /// Exchanges a code for tokens. Unknown, consumed, expired, another
    /// client's, redirect-mismatched and verifier-mismatched codes are all
    /// `InvalidGrant`.
    pub fn exchange_code(
        &self,
        client: &ClientAuth<'_>,
        exchange: &CodeExchange<'_>,
    ) -> Result<TokenGrant, OAuthError> {
        let lifetimes = self.lifetimes;
        self.with_state(|state, now_ms| {
            let client_id = state.authenticate(client)?.id.clone();
            let code = state
                .codes
                .find_mut(exchange.code)
                .ok_or(OAuthError::InvalidGrant)?;
            if code.consumed {
                if let Some(family) = code.family {
                    state.revoke_family(family);
                }
                return Err(OAuthError::InvalidGrant);
            }
            code.consumed = true;
            let verified = pkce::transform(exchange.code_verifier).is_some_and(|challenge| {
                secret::verifies(&code.challenge_digest, CHALLENGE_LABEL, &challenge)
            });
            let valid = now_ms < code.expires_ms
                && code.client == client_id
                && code.redirect_uri.as_str() == exchange.redirect_uri
                && verified;
            if !valid {
                return Err(OAuthError::InvalidGrant);
            }
            state.next_family += 1;
            let family = state.next_family;
            code.family = Some(family);
            let subject = code.subject.clone();
            let scopes = code.scopes.clone();
            let grant = Grant {
                client: &client_id,
                subject: &subject,
                scopes: &scopes,
            };
            state.issue_tokens(&lifetimes, now_ms, family, &grant)
        })
    }
}
