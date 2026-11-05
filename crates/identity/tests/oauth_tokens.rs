//! The authorization server fixture's tokens: refresh rotation and family
//! revocation on reuse, introspection, revocation, and redaction of every
//! secret from errors and `Debug` output.
use std::sync::{Arc, Barrier};
use std::thread;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use foundry_identity::oauth::{
    AuthorizationRequest, AuthorizationServer, ClientAuth, ClientRegistration, CodeExchange,
    Introspection, Lifetimes, OAuthError, S256, TokenGrant, TokenKind,
};
use foundry_identity::store::ManualClock;
use sha2::{Digest, Sha256};

const ISSUER: &str = "http://[::1]:8400";
const REDIRECT: &str = "http://[::1]:53682/callback";
const VERIFIER: &str = "fixture-verifier-0123456789-abcdefghijklmnopqrstuvwxyz";
const APP: ClientAuth<'static> = ClientAuth {
    id: "app",
    secret: None,
};

struct Fixture {
    server: AuthorizationServer,
    clock: Arc<ManualClock>,
    resource_secret: String,
}

impl Fixture {
    fn new() -> Self {
        let clock = Arc::new(ManualClock::default());
        let server = AuthorizationServer::new(ISSUER, Lifetimes::default(), clock.clone()).unwrap();
        for id in ["app", "other"] {
            let registration = ClientRegistration {
                id: id.to_owned(),
                redirect_uris: vec![REDIRECT.to_owned()],
                scopes: vec!["read".to_owned()],
                confidential: false,
            };
            server.register_client(registration).unwrap();
        }
        let resource_secret = server
            .register_client(ClientRegistration {
                id: "resource".to_owned(),
                redirect_uris: Vec::new(),
                scopes: Vec::new(),
                confidential: true,
            })
            .unwrap()
            .unwrap();
        Self {
            server,
            clock,
            resource_secret: resource_secret.expose().to_owned(),
        }
    }

    fn resource(&self) -> ClientAuth<'_> {
        ClientAuth::confidential("resource", &self.resource_secret)
    }

    fn active(&self, token: &str) -> bool {
        let introspection = self.server.introspect(&self.resource(), token).unwrap();
        matches!(introspection, Introspection::Active(_))
    }

    fn tokens(&self) -> TokenGrant {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(VERIFIER.as_bytes()));
        let request = AuthorizationRequest {
            client_id: "app",
            redirect_uri: REDIRECT,
            scope: "read",
            state: None,
            code_challenge: Some(&challenge),
            code_challenge_method: Some(S256),
        };
        let code = self.server.authorize(&request, "user-a").unwrap().code;
        let exchange = CodeExchange {
            code: code.expose(),
            redirect_uri: REDIRECT,
            code_verifier: VERIFIER,
        };
        self.server.exchange_code(&APP, &exchange).unwrap()
    }
}

#[test]
fn refresh_rotates_and_reusing_the_old_token_revokes_the_family() {
    let fixture = Fixture::new();
    let first = fixture.tokens();
    let second = fixture
        .server
        .refresh(&APP, first.refresh_token.expose())
        .unwrap();
    assert_ne!(second.refresh_token, first.refresh_token);
    assert!(fixture.active(second.access_token.expose()));
    assert!(!fixture.active(first.refresh_token.expose()));
    // The first access token stays valid until its own expiry.
    assert!(fixture.active(first.access_token.expose()));

    let reuse = fixture.server.refresh(&APP, first.refresh_token.expose());
    assert_eq!(reuse.unwrap_err(), OAuthError::InvalidGrant);
    for token in [
        &first.access_token,
        &second.access_token,
        &second.refresh_token,
    ] {
        assert!(!fixture.active(token.expose()));
    }
    let after = fixture.server.refresh(&APP, second.refresh_token.expose());
    assert_eq!(after.unwrap_err(), OAuthError::InvalidGrant);

    // Other families are untouched.
    let unrelated = fixture.tokens();
    assert!(
        fixture
            .server
            .refresh(&APP, unrelated.refresh_token.expose())
            .is_ok()
    );
}

#[test]
fn revoked_refresh_token_is_rejected_with_its_family() {
    let fixture = Fixture::new();
    let tokens = fixture.tokens();
    fixture
        .server
        .revoke(&APP, tokens.refresh_token.expose())
        .unwrap();
    let refreshed = fixture.server.refresh(&APP, tokens.refresh_token.expose());
    assert_eq!(refreshed.unwrap_err(), OAuthError::InvalidGrant);
    assert!(!fixture.active(tokens.access_token.expose()));
}

#[test]
fn revoking_an_access_token_leaves_the_refresh_token_usable() {
    let fixture = Fixture::new();
    let tokens = fixture.tokens();
    fixture
        .server
        .revoke(&APP, tokens.access_token.expose())
        .unwrap();
    assert!(!fixture.active(tokens.access_token.expose()));
    assert!(
        fixture
            .server
            .refresh(&APP, tokens.refresh_token.expose())
            .is_ok()
    );
}

#[test]
fn refresh_by_another_client_or_after_expiry_is_rejected() {
    let fixture = Fixture::new();
    let tokens = fixture.tokens();
    let other = fixture
        .server
        .refresh(&ClientAuth::public("other"), tokens.refresh_token.expose());
    assert_eq!(other.unwrap_err(), OAuthError::InvalidGrant);
    // The other client's attempt revoked nothing.
    assert!(fixture.active(tokens.refresh_token.expose()));
    let access = fixture.server.refresh(&APP, tokens.access_token.expose());
    assert_eq!(access.unwrap_err(), OAuthError::InvalidGrant);

    fixture.clock.advance(86_400_000);
    let expired = fixture.server.refresh(&APP, tokens.refresh_token.expose());
    assert_eq!(expired.unwrap_err(), OAuthError::InvalidGrant);
}

#[test]
fn introspection_needs_a_confidential_client_and_hides_inactive_tokens() {
    let fixture = Fixture::new();
    let tokens = fixture.tokens();
    let access = tokens.access_token.expose();
    assert_eq!(
        fixture.server.introspect(&APP, access).unwrap_err(),
        OAuthError::InvalidClient
    );
    let wrong = ClientAuth::confidential("resource", "WRONG");
    assert_eq!(
        fixture.server.introspect(&wrong, access).unwrap_err(),
        OAuthError::InvalidClient
    );
    let missing = ClientAuth::public("resource");
    assert_eq!(
        fixture.server.introspect(&missing, access).unwrap_err(),
        OAuthError::InvalidClient
    );

    let Introspection::Active(active) = fixture
        .server
        .introspect(&fixture.resource(), access)
        .unwrap()
    else {
        panic!("access token is inactive");
    };
    assert_eq!(active.kind, TokenKind::Access);
    assert_eq!((active.issued_ms, active.expires_ms), (0, 600_000));
    for unknown in [
        "",
        "not-a-token",
        "AAAAAAAA-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        assert!(!fixture.active(unknown));
    }
    fixture.clock.advance(600_000);
    assert!(!fixture.active(access));
}

#[test]
fn only_the_issued_client_may_revoke_and_unknown_tokens_succeed() {
    let fixture = Fixture::new();
    let tokens = fixture.tokens();
    let other = fixture
        .server
        .revoke(&ClientAuth::public("other"), tokens.refresh_token.expose());
    assert_eq!(other.unwrap_err(), OAuthError::UnauthorizedClient);
    assert!(fixture.active(tokens.refresh_token.expose()));
    assert!(fixture.server.revoke(&APP, "not-a-token").is_ok());
    let unknown = fixture.server.revoke(&ClientAuth::public("nobody"), "x");
    assert_eq!(unknown.unwrap_err(), OAuthError::InvalidClient);
}

#[test]
fn a_refresh_token_presented_concurrently_rotates_once_then_the_family_dies() {
    const RACERS: usize = 8;
    let fixture = Arc::new(Fixture::new());
    let refresh = Arc::new(fixture.tokens().refresh_token.expose().to_owned());
    let barrier = Arc::new(Barrier::new(RACERS));
    let racers: Vec<_> = (0..RACERS)
        .map(|_| {
            let (fixture, refresh, barrier) = (fixture.clone(), refresh.clone(), barrier.clone());
            thread::spawn(move || {
                barrier.wait();
                fixture.server.refresh(&APP, &refresh)
            })
        })
        .collect();
    let outcomes: Vec<_> = racers
        .into_iter()
        .map(|racer| racer.join().unwrap())
        .collect();
    let winners: Vec<_> = outcomes
        .iter()
        .filter_map(|outcome| outcome.as_ref().ok())
        .collect();
    assert_eq!(winners.len(), 1);
    // Every loser presented a rotated token, so the winner's tokens are revoked.
    assert!(!fixture.active(winners[0].refresh_token.expose()));
    assert!(!fixture.active(winners[0].access_token.expose()));
}

#[test]
fn no_secret_reaches_errors_or_debug_output() {
    let fixture = Fixture::new();
    let first = fixture.tokens();
    let second = fixture
        .server
        .refresh(&APP, first.refresh_token.expose())
        .unwrap();
    let device = fixture.server.device_authorization(&APP, "read").unwrap();
    let secrets = [
        first.access_token.expose(),
        first.refresh_token.expose(),
        second.access_token.expose(),
        second.refresh_token.expose(),
        device.device_code.expose(),
        device.user_code.expose(),
        &fixture.resource_secret,
        VERIFIER,
    ];
    let failures = [
        fixture
            .server
            .refresh(&APP, first.refresh_token.expose())
            .unwrap_err(),
        fixture
            .server
            .introspect(&ClientAuth::confidential("resource", VERIFIER), "x")
            .unwrap_err(),
        fixture
            .server
            .exchange_device_code(&APP, device.device_code.expose())
            .unwrap_err(),
    ];
    let mut rendered = vec![
        format!("{first:?}"),
        format!("{second:?}"),
        format!("{device:?}"),
        format!("{:?}", fixture.server),
    ];
    for failure in &failures {
        rendered.push(format!("{failure:?}"));
        rendered.push(failure.to_string());
    }
    for text in &rendered {
        for secret in secrets {
            assert!(!text.contains(secret), "{text}");
        }
    }
    assert!(rendered[0].contains("<redacted>"));
}
