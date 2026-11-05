//! A local OAuth authorization server fixture for tests: the authorization
//! code grant with `S256` PKCE, device authorization, refresh-token rotation
//! with family revocation on reuse, introspection and revocation, all driven
//! by an injected [`Clock`] such as [`ManualClock`].
//!
//! The fixture is in-process. It binds no socket and calls no identity
//! provider; the only URIs it accepts or produces are loopback `http` URIs
//! ([`LoopbackUri`]). Binding it to an HTTP transport waits on NIC-36 C2, and
//! whether a Foundry-owned authority like this one backs real logins is
//! decision C1, so it is not wired into the credential store or the guard.
//! All parameter, error, token-type and path names are provisional pending
//! NIC-34 D1 and D6.
//!
//! Every operation runs under one lock and reads the clock after taking it,
//! so codes, device codes and refresh tokens are consumed at most once even
//! under concurrent presentation.
//!
//! Known limitations:
//! - State is in memory only; nothing outlives the server.
//! - The user's approval is a direct call with a subject; there is no login
//!   or consent page, and no attempt limit on user codes.
//! - Errors are returned to the caller. Redirecting an error to the client's
//!   redirect URI is left to a transport.
//! - Lifetimes are provisional defaults; the specification gives none.
//!
//! [`Clock`]: crate::store::Clock
//! [`ManualClock`]: crate::store::ManualClock
mod code;
mod device;
mod error;
mod ledger;
mod pkce;
mod redirect;
mod token;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use code::{AuthorizationCode, AuthorizationRequest, CodeExchange};
pub use device::DeviceAuthorization;
pub use error::OAuthError;
pub use pkce::S256;
pub use redirect::LoopbackUri;
pub use token::{ActiveToken, Introspection, TokenGrant, TokenKind};

use crate::store::{Clock, SecretText, secret};
use ledger::Ledger;

/// 52 symbols of five bits each: 260 random bits, as for scoped keys.
const CLIENT_SECRET_SYMBOLS: usize = 52;

/// Provisional lifetimes for each issued artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lifetimes {
    pub code: Duration,
    pub access: Duration,
    pub refresh: Duration,
    pub device: Duration,
    /// The shortest wait between device-code polls; each early poll adds
    /// five seconds to it.
    pub device_interval: Duration,
}

impl Default for Lifetimes {
    fn default() -> Self {
        Self {
            code: Duration::from_secs(60),
            access: Duration::from_secs(600),
            refresh: Duration::from_secs(86_400),
            device: Duration::from_secs(600),
            device_interval: Duration::from_secs(5),
        }
    }
}

/// A client to register. A confidential client receives a secret and may
/// introspect; a public client authenticates by identifier alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRegistration {
    pub id: String,
    /// Exact loopback redirect URIs; may be empty for a device-only client.
    pub redirect_uris: Vec<String>,
    /// The scopes the client may request.
    pub scopes: Vec<String>,
    pub confidential: bool,
}

/// Client authentication as presented with a request. It has no `Debug`, so
/// the secret cannot reach a log through it.
#[derive(Clone, Copy)]
pub struct ClientAuth<'a> {
    pub id: &'a str,
    pub secret: Option<&'a str>,
}

impl<'a> ClientAuth<'a> {
    pub fn public(id: &'a str) -> Self {
        Self { id, secret: None }
    }

    pub fn confidential(id: &'a str, secret: &'a str) -> Self {
        Self {
            id,
            secret: Some(secret),
        }
    }
}

struct Client {
    id: String,
    /// Digest of the secret bound to the client identifier.
    secret_digest: Option<[u8; 32]>,
    redirect_uris: Vec<LoopbackUri>,
    scopes: BTreeSet<String>,
}

impl Client {
    /// The requested scopes, which must be well formed and registered.
    fn grantable(&self, requested: &str) -> Result<BTreeSet<String>, OAuthError> {
        let scopes = requested
            .split(' ')
            .filter(|name| !name.is_empty())
            .map(|name| scope_name(name).ok_or(OAuthError::InvalidScope))
            .collect::<Result<BTreeSet<_>, _>>()?;
        if !scopes.is_subset(&self.scopes) {
            return Err(OAuthError::InvalidScope);
        }
        Ok(scopes)
    }
}

/// A scope token: printable ASCII without space, `"` or `\`.
fn scope_name(name: &str) -> Option<String> {
    let valid = name
        .bytes()
        .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\');
    (!name.is_empty() && valid).then(|| name.to_owned())
}

#[derive(Default)]
struct State {
    clients: HashMap<String, Client>,
    codes: Ledger<code::CodeGrant>,
    devices: Ledger<device::DeviceGrant>,
    /// Digest of each live user code, mapped to its device grant's prefix.
    user_codes: HashMap<[u8; 32], String>,
    tokens: Ledger<token::TokenRecord>,
    revoked_families: HashSet<u64>,
    /// The last token family started; each code or device grant starts one.
    next_family: u64,
}

impl State {
    /// The registered client `auth` names, if its authentication is right.
    /// A public client must not present a secret; a confidential one must.
    fn authenticate(&self, auth: &ClientAuth<'_>) -> Result<&Client, OAuthError> {
        let client = self.clients.get(auth.id).ok_or(OAuthError::InvalidClient)?;
        let authenticated = match (&client.secret_digest, auth.secret) {
            (None, None) => true,
            (Some(digest), Some(presented)) => {
                secret::verifies(digest.as_slice(), &client.id, presented)
            }
            _ => false,
        };
        authenticated
            .then_some(client)
            .ok_or(OAuthError::InvalidClient)
    }
}

/// The fixture. All methods take `&self` and may be called from many threads.
pub struct AuthorizationServer {
    issuer: LoopbackUri,
    lifetimes: Lifetimes,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

impl std::fmt::Debug for AuthorizationServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorizationServer")
            .field("issuer", &self.issuer)
            .field("lifetimes", &self.lifetimes)
            .finish_non_exhaustive()
    }
}

impl AuthorizationServer {
    /// A server identified by the loopback `issuer`.
    pub fn new(
        issuer: &str,
        lifetimes: Lifetimes,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, OAuthError> {
        let issuer =
            LoopbackUri::parse(issuer).ok_or(OAuthError::InvalidRequest { field: "issuer" })?;
        Ok(Self {
            issuer,
            lifetimes,
            clock,
            state: Mutex::new(State::default()),
        })
    }

    pub fn issuer(&self) -> &LoopbackUri {
        &self.issuer
    }

    /// Registers a client. Returns the client secret for a confidential
    /// client; it is the only copy.
    pub fn register_client(
        &self,
        registration: ClientRegistration,
    ) -> Result<Option<SecretText>, OAuthError> {
        if registration.id.is_empty() {
            return Err(OAuthError::InvalidRequest { field: "client_id" });
        }
        let redirect_uris = registration
            .redirect_uris
            .iter()
            .map(|uri| LoopbackUri::parse(uri))
            .collect::<Option<Vec<_>>>()
            .ok_or(OAuthError::InvalidRequest {
                field: "redirect_uri",
            })?;
        let scopes = registration
            .scopes
            .iter()
            .map(|name| scope_name(name))
            .collect::<Option<BTreeSet<_>>>()
            .ok_or(OAuthError::InvalidScope)?;
        self.with_state(|state, _| {
            if state.clients.contains_key(&registration.id) {
                return Err(OAuthError::InvalidRequest { field: "client_id" });
            }
            let issued = if registration.confidential {
                Some(SecretText::new(secret::random_symbols(
                    CLIENT_SECRET_SYMBOLS,
                )?))
            } else {
                None
            };
            let secret_digest = issued
                .as_ref()
                .map(|text| secret::digest(&registration.id, text.expose()));
            state.clients.insert(
                registration.id.clone(),
                Client {
                    id: registration.id,
                    secret_digest,
                    redirect_uris,
                    scopes,
                },
            );
            Ok(issued)
        })
    }

    /// Runs `operation` under the state lock with the time read after it.
    fn with_state<T>(
        &self,
        operation: impl FnOnce(&mut State, u64) -> Result<T, OAuthError>,
    ) -> Result<T, OAuthError> {
        let mut state = self.state.lock().map_err(|_| OAuthError::Unavailable {
            detail: "state lock poisoned".to_owned(),
        })?;
        let now_ms = self.clock.now_ms();
        operation(&mut state, now_ms)
    }
}

/// `now_ms` plus `lifetime`, saturating at the end of the time line.
fn deadline(now_ms: u64, lifetime: Duration) -> u64 {
    now_ms.saturating_add(u64::try_from(lifetime.as_millis()).unwrap_or(u64::MAX))
}
