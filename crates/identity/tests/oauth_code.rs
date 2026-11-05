//! The authorization server fixture's code grant: `S256` PKCE, loopback
//! redirect matching, replay and expiry, and single use of codes under
//! concurrent presentation.
use std::sync::{Arc, Barrier};
use std::thread;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use foundry_identity::oauth::{
    AuthorizationRequest, AuthorizationServer, ClientAuth, ClientRegistration, CodeExchange,
    Introspection, Lifetimes, OAuthError, S256, TokenGrant,
};
use foundry_identity::store::ManualClock;
use sha2::{Digest, Sha256};

const ISSUER: &str = "http://127.0.0.1:8400";
const REDIRECT: &str = "http://127.0.0.1:53682/callback";
const VERIFIER: &str = "fixture-verifier-0123456789-abcdefghijklmnopqrstuvwxyz";
const OTHER_VERIFIER: &str = "fixture-verifier-9876543210-zyxwvutsrqponmlkjihgfedcba";

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
            let registration = client(id, &[REDIRECT], false);
            assert!(server.register_client(registration).unwrap().is_none());
        }
        let resource = server
            .register_client(client("resource", &[], true))
            .unwrap()
            .unwrap();
        Self {
            server,
            clock,
            resource_secret: resource.expose().to_owned(),
        }
    }

    fn introspect(&self, token: &str) -> Introspection {
        let caller = ClientAuth::confidential("resource", &self.resource_secret);
        self.server.introspect(&caller, token).unwrap()
    }

    fn code(&self, verifier: &str) -> String {
        let challenge = challenge(verifier);
        let issued = self
            .server
            .authorize(&request(&challenge), "user-a")
            .unwrap();
        issued.code.expose().to_owned()
    }

    fn exchange(&self, client: &str, code: &str, verifier: &str) -> Result<TokenGrant, OAuthError> {
        let exchange = CodeExchange {
            code,
            redirect_uri: REDIRECT,
            code_verifier: verifier,
        };
        self.server
            .exchange_code(&ClientAuth::public(client), &exchange)
    }
}

fn client(id: &str, redirect_uris: &[&str], confidential: bool) -> ClientRegistration {
    ClientRegistration {
        id: id.to_owned(),
        redirect_uris: redirect_uris.iter().map(|uri| (*uri).to_owned()).collect(),
        scopes: vec!["read".to_owned(), "write".to_owned()],
        confidential,
        introspector: confidential,
    }
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn request(challenge: &str) -> AuthorizationRequest<'_> {
    AuthorizationRequest {
        client_id: "app",
        redirect_uri: REDIRECT,
        scope: "read",
        state: Some("opaque-state"),
        code_challenge: Some(challenge),
        code_challenge_method: Some(S256),
    }
}

#[test]
fn code_flow_issues_tokens_bound_to_subject_client_and_scope() {
    let fixture = Fixture::new();
    let challenge = challenge(VERIFIER);
    let issued = fixture
        .server
        .authorize(&request(&challenge), "user-a")
        .unwrap();
    assert_eq!(issued.redirect_uri.as_str(), REDIRECT);
    assert_eq!(issued.state.as_deref(), Some("opaque-state"));

    let tokens = fixture
        .exchange("app", issued.code.expose(), VERIFIER)
        .unwrap();
    assert_eq!(tokens.token_type, "Bearer");
    assert_eq!(tokens.access_expires_ms, 600_000);
    let Introspection::Active(active) = fixture.introspect(tokens.access_token.expose()) else {
        panic!("access token is inactive");
    };
    assert_eq!(active.client_id, "app");
    assert_eq!(active.subject, "user-a");
    assert_eq!(active.scopes, ["read".to_owned()].into());
}

#[test]
fn plain_absent_or_malformed_challenges_are_refused() {
    let fixture = Fixture::new();
    let challenge = challenge(VERIFIER);
    let method_error = OAuthError::InvalidRequest {
        field: "code_challenge_method",
    };
    for method in [Some("plain"), Some("s256"), None] {
        let refused = AuthorizationRequest {
            code_challenge_method: method,
            ..request(&challenge)
        };
        assert_eq!(
            fixture.server.authorize(&refused, "user-a").unwrap_err(),
            method_error
        );
    }
    let challenge_error = OAuthError::InvalidRequest {
        field: "code_challenge",
    };
    for bad in [None, Some(VERIFIER), Some(&challenge[1..])] {
        let refused = AuthorizationRequest {
            code_challenge: bad,
            ..request(&challenge)
        };
        assert_eq!(
            fixture.server.authorize(&refused, "user-a").unwrap_err(),
            challenge_error
        );
    }
}

#[test]
fn wrong_verifier_is_rejected_and_uses_up_the_code() {
    let fixture = Fixture::new();
    let code = fixture.code(VERIFIER);
    for verifier in [OTHER_VERIFIER, "short", &challenge(VERIFIER)] {
        let code = fixture.code(VERIFIER);
        let refused = fixture.exchange("app", &code, verifier).unwrap_err();
        assert_eq!(refused, OAuthError::InvalidGrant);
    }
    assert!(fixture.exchange("app", &code, OTHER_VERIFIER).is_err());
    assert_eq!(
        fixture.exchange("app", &code, VERIFIER).unwrap_err(),
        OAuthError::InvalidGrant
    );
}

#[test]
fn replayed_code_is_rejected_and_revokes_the_tokens_it_issued() {
    let fixture = Fixture::new();
    let code = fixture.code(VERIFIER);
    let tokens = fixture.exchange("app", &code, VERIFIER).unwrap();
    assert_eq!(
        fixture.exchange("app", &code, VERIFIER).unwrap_err(),
        OAuthError::InvalidGrant
    );
    assert_eq!(
        fixture.introspect(tokens.access_token.expose()),
        Introspection::Inactive
    );
    let refreshed = fixture
        .server
        .refresh(&ClientAuth::public("app"), tokens.refresh_token.expose());
    assert_eq!(refreshed.unwrap_err(), OAuthError::InvalidGrant);
}

#[test]
fn expired_code_is_rejected() {
    let fixture = Fixture::new();
    let code = fixture.code(VERIFIER);
    fixture.clock.advance(60_000);
    assert_eq!(
        fixture.exchange("app", &code, VERIFIER).unwrap_err(),
        OAuthError::InvalidGrant
    );
}

#[test]
fn redirect_uri_must_match_host_path_and_query_exactly() {
    let fixture = Fixture::new();
    let challenge = challenge(VERIFIER);
    let near_misses = [
        "http://127.0.0.1:53682/callback/",
        "http://127.0.0.1:53682/Callback",
        "http://127.0.0.1:53682/callbac",
        "https://127.0.0.1:53682/callback",
        "http://127.0.0.1:53682/callback?extra=1",
        "http://localhost:53682/callback",
        "http://[::1]:53682/callback",
    ];
    for redirect_uri in near_misses {
        let refused = AuthorizationRequest {
            redirect_uri,
            ..request(&challenge)
        };
        assert_eq!(
            fixture.server.authorize(&refused, "user-a").unwrap_err(),
            OAuthError::InvalidRequest {
                field: "redirect_uri"
            },
            "{redirect_uri}"
        );
    }
    let code = fixture.code(VERIFIER);
    let exchange = CodeExchange {
        code: &code,
        redirect_uri: near_misses[0],
        code_verifier: VERIFIER,
    };
    let refused = fixture
        .server
        .exchange_code(&ClientAuth::public("app"), &exchange);
    assert_eq!(refused.unwrap_err(), OAuthError::InvalidGrant);
}

#[test]
fn loopback_redirects_may_use_any_port_but_the_exchange_repeats_it() {
    let fixture = Fixture::new();
    let challenge = challenge(VERIFIER);
    for redirect_uri in [
        "http://127.0.0.1:61000/callback",
        "http://127.0.0.1/callback",
    ] {
        let moved = AuthorizationRequest {
            redirect_uri,
            ..request(&challenge)
        };
        let issued = fixture.server.authorize(&moved, "user-a").unwrap();
        assert_eq!(issued.redirect_uri.as_str(), redirect_uri);
        // The registered port is not a substitute for the one requested.
        let registered = CodeExchange {
            code: issued.code.expose(),
            redirect_uri: REDIRECT,
            code_verifier: VERIFIER,
        };
        let refused = fixture
            .server
            .exchange_code(&ClientAuth::public("app"), &registered);
        assert_eq!(refused.unwrap_err(), OAuthError::InvalidGrant);

        let issued = fixture.server.authorize(&moved, "user-a").unwrap();
        let repeated = CodeExchange {
            code: issued.code.expose(),
            redirect_uri,
            code_verifier: VERIFIER,
        };
        assert!(
            fixture
                .server
                .exchange_code(&ClientAuth::public("app"), &repeated)
                .is_ok()
        );
    }
}

#[test]
fn only_loopback_redirect_and_issuer_uris_are_accepted() {
    let fixture = Fixture::new();
    let refused = [
        "https://127.0.0.1:8080/cb",
        "http://example.com/cb",
        "http://localhost:8080/cb",
        "http://127.0.0.1.example.com/cb",
        "http://127.0.0.1@example.com/cb",
        "http://user@127.0.0.1:8080/cb",
        "http://127.0.0.2:8080/cb",
        "http://127.0.0.1:0/cb",
        "http://127.0.0.1:65536/cb",
        "http://127.0.0.1:/cb",
        "http://127.0.0.1:8080/cb#fragment",
        "http://127.0.0.1:8080/c b",
        "http://[::2]:8080/cb",
    ];
    for (index, uri) in refused.into_iter().enumerate() {
        let registration = client(&format!("client-{index}"), &[uri], false);
        assert_eq!(
            fixture.server.register_client(registration).unwrap_err(),
            OAuthError::InvalidRequest {
                field: "redirect_uri"
            },
            "{uri}"
        );
        let clock = Arc::new(ManualClock::default());
        assert!(AuthorizationServer::new(uri, Lifetimes::default(), clock).is_err());
    }
    for (index, uri) in [
        "http://127.0.0.1/cb",
        "http://[::1]:8080/cb",
        "http://[::1]",
    ]
    .into_iter()
    .enumerate()
    {
        let registration = client(&format!("loopback-{index}"), &[uri], false);
        assert!(
            fixture.server.register_client(registration).is_ok(),
            "{uri}"
        );
    }
}

#[test]
fn another_or_unknown_client_cannot_exchange_or_use_up_the_code() {
    let fixture = Fixture::new();
    let code = fixture.code(VERIFIER);
    assert_eq!(
        fixture.exchange("nobody", &code, VERIFIER).unwrap_err(),
        OAuthError::InvalidClient
    );
    assert_eq!(
        fixture.exchange("other", &code, VERIFIER).unwrap_err(),
        OAuthError::InvalidGrant
    );
    let tokens = fixture.exchange("app", &code, VERIFIER).unwrap();
    // Another client presenting a used code is not a replay by its holder.
    assert_eq!(
        fixture.exchange("other", &code, VERIFIER).unwrap_err(),
        OAuthError::InvalidGrant
    );
    assert!(matches!(
        fixture.introspect(tokens.access_token.expose()),
        Introspection::Active(_)
    ));
    let challenge = challenge(VERIFIER);
    let unknown = AuthorizationRequest {
        client_id: "nobody",
        ..request(&challenge)
    };
    assert_eq!(
        fixture.server.authorize(&unknown, "user-a").unwrap_err(),
        OAuthError::InvalidClient
    );
}

#[test]
fn a_code_presented_concurrently_is_exchanged_exactly_once() {
    const RACERS: usize = 16;
    let fixture = Arc::new(Fixture::new());
    let code = Arc::new(fixture.code(VERIFIER));
    let barrier = Arc::new(Barrier::new(RACERS));
    let racers: Vec<_> = (0..RACERS)
        .map(|_| {
            let (fixture, code, barrier) = (fixture.clone(), code.clone(), barrier.clone());
            thread::spawn(move || {
                barrier.wait();
                fixture.exchange("app", &code, VERIFIER)
            })
        })
        .collect();
    let outcomes: Vec<_> = racers
        .into_iter()
        .map(|racer| racer.join().unwrap())
        .collect();
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert!(
        outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref().err())
            .all(|error| *error == OAuthError::InvalidGrant)
    );
}
