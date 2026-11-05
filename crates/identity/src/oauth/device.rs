//! Device authorization. The device polls with its device code while the user
//! approves or denies the user code elsewhere. A poll sooner than the current
//! interval is `SlowDown` and lengthens the interval by five seconds. The
//! user code is live only until the user decides or the grant expires; the
//! device code is consumed by the exchange that issues tokens, and presenting
//! it again revokes them.
use std::collections::BTreeSet;
use std::time::Duration;

use super::error::OAuthError;
use super::token::{Grant, TokenGrant};
use super::{AuthorizationServer, ClientAuth, State, deadline};
use crate::store::{SecretText, secret};

/// Eight symbols of five bits each, shown as two groups of four for typing.
const USER_CODE_SYMBOLS: usize = 8;
/// Binds user-code digests to a fixed label, since they have no prefix.
const USER_CODE_LABEL: &str = "device.user_code";
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);
/// Provisional path of the verification page under the issuer.
const VERIFICATION_PATH: &str = "/device";

enum Decision {
    Pending,
    Approved { subject: String },
    Denied,
}

pub(super) struct DeviceGrant {
    client: String,
    scopes: BTreeSet<String>,
    expires_ms: u64,
    interval: Duration,
    last_poll_ms: Option<u64>,
    decision: Decision,
    consumed: bool,
    family: Option<u64>,
}

/// The response to a device authorization request.
#[derive(Debug, Clone)]
pub struct DeviceAuthorization {
    pub device_code: SecretText,
    /// Shown to the user as `XXXX-XXXX`.
    pub user_code: SecretText,
    pub verification_uri: String,
    pub expires_ms: u64,
    pub interval: Duration,
}

fn user_code_key(user_code: &str) -> Option<[u8; 32]> {
    let symbols: String = user_code.chars().filter(|symbol| *symbol != '-').collect();
    secret::is_symbols(&symbols, USER_CODE_SYMBOLS)
        .then(|| secret::digest(USER_CODE_LABEL, &symbols))
}

impl State {
    /// The pending, unexpired grant `user_code` names. The user code is
    /// retired either way, so it can be decided only once.
    fn take_pending(&mut self, user_code: &str, now_ms: u64) -> Option<&mut DeviceGrant> {
        let prefix = self.user_codes.remove(&user_code_key(user_code)?)?;
        self.devices
            .by_prefix_mut(&prefix)
            .filter(|grant| matches!(grant.decision, Decision::Pending))
            .filter(|grant| now_ms < grant.expires_ms)
    }
}

impl AuthorizationServer {
    /// Starts a device grant for `client` and the requested scopes.
    pub fn device_authorization(
        &self,
        client: &ClientAuth<'_>,
        scope: &str,
    ) -> Result<DeviceAuthorization, OAuthError> {
        let lifetimes = self.lifetimes;
        let verification_uri = format!(
            "{}{VERIFICATION_PATH}",
            self.issuer.as_str().trim_end_matches('/')
        );
        self.with_state(|state, now_ms| {
            let client = state.authenticate(client)?;
            let grant = DeviceGrant {
                client: client.id.clone(),
                scopes: client.grantable(scope)?,
                expires_ms: deadline(now_ms, lifetimes.device),
                interval: lifetimes.device_interval,
                last_poll_ms: None,
                decision: Decision::Pending,
                consumed: false,
                family: None,
            };
            let expires_ms = grant.expires_ms;
            let user_code = loop {
                let candidate = secret::random_symbols(USER_CODE_SYMBOLS)?;
                let key = secret::digest(USER_CODE_LABEL, &candidate);
                if !state.user_codes.contains_key(&key) {
                    break candidate;
                }
            };
            let (prefix, device_code) = state.devices.issue(grant)?;
            state
                .user_codes
                .insert(secret::digest(USER_CODE_LABEL, &user_code), prefix);
            let (first, second) = user_code.split_at(USER_CODE_SYMBOLS / 2);
            Ok(DeviceAuthorization {
                device_code,
                user_code: SecretText::new(format!("{first}-{second}")),
                verification_uri,
                expires_ms,
                interval: lifetimes.device_interval,
            })
        })
    }

    /// The user `subject` approves the grant `user_code` names. An unknown,
    /// expired or already decided user code is `InvalidGrant`.
    pub fn approve_device(&self, user_code: &str, subject: &str) -> Result<(), OAuthError> {
        if subject.is_empty() {
            return Err(OAuthError::InvalidRequest { field: "subject" });
        }
        self.with_state(|state, now_ms| {
            let grant = state
                .take_pending(user_code, now_ms)
                .ok_or(OAuthError::InvalidGrant)?;
            grant.decision = Decision::Approved {
                subject: subject.to_owned(),
            };
            Ok(())
        })
    }

    /// The user denies the grant `user_code` names.
    pub fn deny_device(&self, user_code: &str) -> Result<(), OAuthError> {
        self.with_state(|state, now_ms| {
            let grant = state
                .take_pending(user_code, now_ms)
                .ok_or(OAuthError::InvalidGrant)?;
            grant.decision = Decision::Denied;
            Ok(())
        })
    }

    /// Polls with a device code. Returns tokens once the user approved.
    pub fn exchange_device_code(
        &self,
        client: &ClientAuth<'_>,
        device_code: &str,
    ) -> Result<TokenGrant, OAuthError> {
        let lifetimes = self.lifetimes;
        self.with_state(|state, now_ms| {
            let client_id = state.authenticate(client)?.id.clone();
            let grant = state
                .devices
                .find_mut(device_code)
                .filter(|grant| grant.client == client_id)
                .ok_or(OAuthError::InvalidGrant)?;
            if grant.consumed {
                if let Some(family) = grant.family {
                    state.revoke_family(family);
                }
                return Err(OAuthError::InvalidGrant);
            }
            if now_ms >= grant.expires_ms {
                return Err(OAuthError::ExpiredToken);
            }
            let interval_ms = u64::try_from(grant.interval.as_millis()).unwrap_or(u64::MAX);
            let early = grant
                .last_poll_ms
                .is_some_and(|last| now_ms.saturating_sub(last) < interval_ms);
            grant.last_poll_ms = Some(now_ms);
            if early {
                grant.interval += SLOW_DOWN_STEP;
                return Err(OAuthError::SlowDown);
            }
            let subject = match &grant.decision {
                Decision::Pending => return Err(OAuthError::AuthorizationPending),
                Decision::Denied => return Err(OAuthError::AccessDenied),
                Decision::Approved { subject } => subject.clone(),
            };
            grant.consumed = true;
            state.next_family += 1;
            let family = state.next_family;
            grant.family = Some(family);
            let scopes = grant.scopes.clone();
            let issued = Grant {
                client: &client_id,
                subject: &subject,
                scopes: &scopes,
            };
            state.issue_tokens(&lifetimes, now_ms, family, &issued)
        })
    }
}
