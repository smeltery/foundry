//! The authorization server fixture's device grant: polling, approval and
//! denial, slow-down, expiry and single use of device codes.
use std::sync::Arc;
use std::time::Duration;

use foundry_identity::oauth::{
    AuthorizationServer, ClientAuth, ClientRegistration, Introspection, Lifetimes, OAuthError,
};
use foundry_identity::store::ManualClock;

const ISSUER: &str = "http://127.0.0.1:8400";

struct Fixture {
    server: AuthorizationServer,
    clock: Arc<ManualClock>,
    resource_secret: String,
}

impl Fixture {
    fn new() -> Self {
        let clock = Arc::new(ManualClock::default());
        let server = AuthorizationServer::new(ISSUER, Lifetimes::default(), clock.clone()).unwrap();
        let mut secrets = ["app", "other", "resource"].map(|id| {
            let registration = ClientRegistration {
                id: id.to_owned(),
                redirect_uris: Vec::new(),
                scopes: vec!["read".to_owned(), "write".to_owned()],
                confidential: id == "resource",
            };
            server.register_client(registration).unwrap()
        });
        let resource_secret = secrets[2].take().unwrap().expose().to_owned();
        Self {
            server,
            clock,
            resource_secret,
        }
    }

    fn introspect(&self, token: &str) -> Introspection {
        let caller = ClientAuth::confidential("resource", &self.resource_secret);
        self.server.introspect(&caller, token).unwrap()
    }
}

#[test]
fn device_flow_waits_for_approval_then_issues_tokens_once() {
    let fixture = Fixture::new();
    let app = ClientAuth::public("app");
    let started = fixture
        .server
        .device_authorization(&app, "read write")
        .unwrap();
    assert_eq!(started.verification_uri, "http://127.0.0.1:8400/device");
    assert_eq!(started.interval, Duration::from_secs(5));
    assert_eq!(started.user_code.expose().len(), 9);
    let device_code = started.device_code.expose();

    let pending = fixture.server.exchange_device_code(&app, device_code);
    assert_eq!(pending.unwrap_err(), OAuthError::AuthorizationPending);
    fixture
        .server
        .approve_device(started.user_code.expose(), "user-a")
        .unwrap();
    assert_eq!(
        fixture
            .server
            .approve_device(started.user_code.expose(), "user-b")
            .unwrap_err(),
        OAuthError::InvalidGrant
    );
    fixture.clock.advance(5_000);
    let tokens = fixture
        .server
        .exchange_device_code(&app, device_code)
        .unwrap();
    let Introspection::Active(active) = fixture.introspect(tokens.access_token.expose()) else {
        panic!("access token is inactive");
    };
    assert_eq!(active.subject, "user-a");
    assert_eq!(active.scopes.len(), 2);

    fixture.clock.advance(5_000);
    let replay = fixture.server.exchange_device_code(&app, device_code);
    assert_eq!(replay.unwrap_err(), OAuthError::InvalidGrant);
    assert_eq!(
        fixture.introspect(tokens.access_token.expose()),
        Introspection::Inactive
    );
}

#[test]
fn early_polls_slow_down_and_lengthen_the_interval() {
    let fixture = Fixture::new();
    let app = ClientAuth::public("app");
    let started = fixture.server.device_authorization(&app, "read").unwrap();
    let device_code = started.device_code.expose();
    let poll = || {
        fixture
            .server
            .exchange_device_code(&app, device_code)
            .unwrap_err()
    };
    assert_eq!(poll(), OAuthError::AuthorizationPending);
    fixture.clock.advance(4_999);
    assert_eq!(poll(), OAuthError::SlowDown);
    // The interval is now ten seconds from the early poll.
    fixture.clock.advance(9_999);
    assert_eq!(poll(), OAuthError::SlowDown);
    fixture.clock.advance(15_000);
    assert_eq!(poll(), OAuthError::AuthorizationPending);
}

#[test]
fn denied_expired_and_foreign_device_codes_are_rejected() {
    let fixture = Fixture::new();
    let app = ClientAuth::public("app");
    let denied = fixture.server.device_authorization(&app, "read").unwrap();
    fixture
        .server
        .deny_device(denied.user_code.expose())
        .unwrap();
    let poll = fixture
        .server
        .exchange_device_code(&app, denied.device_code.expose());
    assert_eq!(poll.unwrap_err(), OAuthError::AccessDenied);

    let foreign = fixture.server.device_authorization(&app, "read").unwrap();
    fixture
        .server
        .approve_device(foreign.user_code.expose(), "user-a")
        .unwrap();
    let other = fixture
        .server
        .exchange_device_code(&ClientAuth::public("other"), foreign.device_code.expose());
    assert_eq!(other.unwrap_err(), OAuthError::InvalidGrant);

    let expired = fixture.server.device_authorization(&app, "read").unwrap();
    fixture.clock.advance(600_000);
    assert_eq!(
        fixture
            .server
            .approve_device(expired.user_code.expose(), "user-a")
            .unwrap_err(),
        OAuthError::InvalidGrant
    );
    let poll = fixture
        .server
        .exchange_device_code(&app, expired.device_code.expose());
    assert_eq!(poll.unwrap_err(), OAuthError::ExpiredToken);
    assert_eq!(
        fixture
            .server
            .device_authorization(&app, "admin")
            .unwrap_err(),
        OAuthError::InvalidScope
    );
}
