//! Access and refresh tokens. Every grant starts a family; each refresh
//! rotates the refresh token within it. Presenting a rotated refresh token
//! again means one copy leaked, so the whole family is revoked: every access
//! and refresh token descended from the same grant stops being active.
use std::collections::{BTreeSet, HashSet};

use super::error::OAuthError;
use super::{AuthorizationServer, ClientAuth, Lifetimes, State, deadline};
use crate::store::SecretText;

/// Provisional token type name.
const BEARER: &str = "Bearer";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Access,
    Refresh,
}

impl TokenKind {
    /// Provisional type-hint names.
    pub fn name(self) -> &'static str {
        match self {
            Self::Access => "access_token",
            Self::Refresh => "refresh_token",
        }
    }
}

pub(super) struct TokenRecord {
    kind: TokenKind,
    family: u64,
    client: String,
    subject: String,
    scopes: BTreeSet<String>,
    issued_ms: u64,
    expires_ms: u64,
    revoked: bool,
    /// A refresh token that has already been exchanged.
    rotated: bool,
}

impl TokenRecord {
    fn active(&self, revoked_families: &HashSet<u64>, now_ms: u64) -> bool {
        !self.revoked
            && !self.rotated
            && now_ms < self.expires_ms
            && !revoked_families.contains(&self.family)
    }
}

/// Tokens issued by a grant or a refresh. The token texts are the only
/// copies, and their `Debug` output is redacted.
#[derive(Debug, Clone)]
pub struct TokenGrant {
    pub access_token: SecretText,
    pub refresh_token: SecretText,
    pub token_type: &'static str,
    pub access_expires_ms: u64,
    pub scopes: BTreeSet<String>,
}

/// What introspection reveals about an active token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveToken {
    pub kind: TokenKind,
    pub client_id: String,
    pub subject: String,
    pub scopes: BTreeSet<String>,
    pub issued_ms: u64,
    pub expires_ms: u64,
}

/// An inactive token reveals nothing else, whether it is unknown, expired,
/// rotated or revoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Introspection {
    Inactive,
    Active(ActiveToken),
}

/// What a grant binds into the tokens it issues.
pub(super) struct Grant<'a> {
    pub(super) client: &'a str,
    pub(super) subject: &'a str,
    pub(super) scopes: &'a BTreeSet<String>,
}

impl State {
    /// Issues an access and a refresh token in `family`.
    pub(super) fn issue_tokens(
        &mut self,
        lifetimes: &Lifetimes,
        now_ms: u64,
        family: u64,
        grant: &Grant<'_>,
    ) -> Result<TokenGrant, OAuthError> {
        let record = |kind, lifetime| TokenRecord {
            kind,
            family,
            client: grant.client.to_owned(),
            subject: grant.subject.to_owned(),
            scopes: grant.scopes.clone(),
            issued_ms: now_ms,
            expires_ms: deadline(now_ms, lifetime),
            revoked: false,
            rotated: false,
        };
        let access = record(TokenKind::Access, lifetimes.access);
        let access_expires_ms = access.expires_ms;
        let refresh = record(TokenKind::Refresh, lifetimes.refresh);
        let (_, access_token) = self.tokens.issue(access)?;
        let (_, refresh_token) = self.tokens.issue(refresh)?;
        Ok(TokenGrant {
            access_token,
            refresh_token,
            token_type: BEARER,
            access_expires_ms,
            scopes: grant.scopes.clone(),
        })
    }

    pub(super) fn revoke_family(&mut self, family: u64) {
        self.revoked_families.insert(family);
    }
}

impl AuthorizationServer {
    /// Exchanges a refresh token for a new access and refresh token in the
    /// same family. The presented token is rotated out; presenting it again
    /// revokes the family. A token issued to another client is refused
    /// without revoking anything, since that client cannot have used it.
    pub fn refresh(
        &self,
        client: &ClientAuth<'_>,
        refresh_token: &str,
    ) -> Result<TokenGrant, OAuthError> {
        let lifetimes = self.lifetimes;
        self.with_state(|state, now_ms| {
            let client_id = state.authenticate(client)?.id.clone();
            let record = state
                .tokens
                .find_mut(refresh_token)
                .filter(|record| record.kind == TokenKind::Refresh)
                .ok_or(OAuthError::InvalidGrant)?;
            if record.client != client_id {
                return Err(OAuthError::InvalidGrant);
            }
            let family = record.family;
            if record.rotated {
                state.revoke_family(family);
                return Err(OAuthError::InvalidGrant);
            }
            if !record.active(&state.revoked_families, now_ms) {
                return Err(OAuthError::InvalidGrant);
            }
            record.rotated = true;
            let subject = record.subject.clone();
            let scopes = record.scopes.clone();
            let grant = Grant {
                client: &client_id,
                subject: &subject,
                scopes: &scopes,
            };
            state.issue_tokens(&lifetimes, now_ms, family, &grant)
        })
    }

    /// Reports whether `token` is active. The caller must authenticate, and
    /// sees an active token only if it was issued to the caller or the caller
    /// is registered as an introspector; anyone else gets `Inactive`, so
    /// introspection never tells one client about another's tokens.
    pub fn introspect(
        &self,
        caller: &ClientAuth<'_>,
        token: &str,
    ) -> Result<Introspection, OAuthError> {
        self.with_state(|state, now_ms| {
            let caller = state.authenticate(caller)?;
            let (caller_id, introspector) = (caller.id.clone(), caller.introspector);
            let Some(record) = state.tokens.find_mut(token) else {
                return Ok(Introspection::Inactive);
            };
            let visible = introspector || record.client == caller_id;
            if !visible || !record.active(&state.revoked_families, now_ms) {
                return Ok(Introspection::Inactive);
            }
            Ok(Introspection::Active(ActiveToken {
                kind: record.kind,
                client_id: record.client.clone(),
                subject: record.subject.clone(),
                scopes: record.scopes.clone(),
                issued_ms: record.issued_ms,
                expires_ms: record.expires_ms,
            }))
        })
    }

    /// Revokes `token` for the client it was issued to. Revoking a refresh
    /// token revokes its whole family; revoking an access token revokes only
    /// that token. An unknown or already inactive token succeeds unchanged.
    pub fn revoke(&self, client: &ClientAuth<'_>, token: &str) -> Result<(), OAuthError> {
        self.with_state(|state, _| {
            let client_id = state.authenticate(client)?.id.clone();
            let Some(record) = state.tokens.find_mut(token) else {
                return Ok(());
            };
            if record.client != client_id {
                return Err(OAuthError::UnauthorizedClient);
            }
            record.revoked = true;
            if record.kind == TokenKind::Refresh {
                let family = record.family;
                state.revoke_family(family);
            }
            Ok(())
        })
    }
}
