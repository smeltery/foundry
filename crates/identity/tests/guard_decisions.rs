//! Guard outcomes: bearer extraction, credential resolution, routing and the
//! decision's 401, 403, 404 and 429 classes, and redaction of token bytes.
use std::collections::BTreeMap;

use foundry_identity::authz::*;
use foundry_identity::guard::*;

const SECRET: &str = "s3cr3t-Token_value~1";
const REVOKED: &str = "revoked-token";
const BOUND: &str = "w-1";

/// An in-memory credential store keyed by bearer bytes.
struct Store(BTreeMap<&'static str, Principal>);

impl CredentialResolver for Store {
    fn resolve(&self, token: &BearerToken) -> Result<Option<Principal>, ResolverUnavailable> {
        Ok(self.0.get(token.secret()).cloned())
    }
}

struct Down;

impl CredentialResolver for Down {
    fn resolve(&self, _token: &BearerToken) -> Result<Option<Principal>, ResolverUnavailable> {
        Err(ResolverUnavailable)
    }
}

struct Limited(u32);

impl Throttle for Limited {
    fn retry_after(&self, _principal: &Principal, _route: Option<&Route>) -> Option<u32> {
        Some(self.0)
    }
}

fn member(scopes: &[Scope]) -> Principal {
    let credential = Credential::new(CredentialKind::LoginGrant, WorkspaceId::new(BOUND))
        .with_scopes(scopes.iter().copied());
    Principal::new(PrincipalId::new("u-1"), credential)
}

fn store(scopes: &[Scope]) -> Store {
    let revoked = Principal {
        credential: member(scopes)
            .credential
            .with_state(CredentialState::Revoked),
        ..member(scopes)
    };
    Store(BTreeMap::from([
        (SECRET, member(scopes)),
        (REVOKED, revoked),
    ]))
}

fn org() -> Organization {
    Organization::new().with_workspace(
        Workspace::new(WorkspaceId::new(BOUND)).with_member(PrincipalId::new("u-1"), Role::Member),
    )
}

fn request<'a>(method: Method, path: &'a str, authorization: &'a [&'a str]) -> GuardRequest<'a> {
    GuardRequest {
        method,
        path,
        authorization,
        new_role: None,
    }
}

fn check(
    method: Method,
    path: &str,
    header: &[&str],
    scopes: &[Scope],
) -> Result<Access, Rejection> {
    guard(
        &request(method, path, header),
        &store(scopes),
        &Unthrottled,
        &org(),
    )
    .map(|admission| admission.access)
}

const BEARER: &[&str] = &["Bearer s3cr3t-Token_value~1"];
const READ: &[Scope] = &[Scope::WorkspaceRead];

#[test]
fn a_scoped_member_is_admitted_to_a_bound_workspace_route() {
    let header = ["bearer   s3cr3t-Token_value~1 "];
    let admission = guard(
        &request(Method::Get, "/v1/workspaces/w-1/members/u-2", &header),
        &store(READ),
        &Unthrottled,
        &org(),
    )
    .expect("admitted");
    assert_eq!(admission.matched.route.operation(), "member.get");
    assert_eq!(admission.matched.param("member"), Some("u-2"));
    assert_eq!(admission.access, Access::Full);
}

#[test]
fn missing_or_malformed_bearers_are_unauthenticated() {
    let cases: [(&[&str], BearerError); 9] = [
        (&[], BearerError::Missing),
        (&[BEARER[0], BEARER[0]], BearerError::Repeated),
        (&["Basic dXNlcjpwYXNz"], BearerError::WrongScheme),
        (&["s3cr3t-Token_value~1"], BearerError::WrongScheme),
        (&[""], BearerError::WrongScheme),
        (&["Bearer"], BearerError::Malformed),
        (&["Bearer  "], BearerError::Malformed),
        (&["Bearer two parts"], BearerError::Malformed),
        (&["Bearer =pad-only"], BearerError::Malformed),
    ];
    for (header, expected) in cases {
        let rejection = check(Method::Get, "/v1/workspaces", header, READ).expect_err("refused");
        assert_eq!(rejection, Rejection::Bearer(expected), "{header:?}");
        assert_eq!(rejection.class(), ErrorClass::Unauthenticated);
        assert_eq!(rejection.provisional_status(), 401);
    }
    assert!(extract_bearer(&["Bearer abc+/=="]).is_ok());
}

#[test]
fn unknown_and_revoked_credentials_are_unauthenticated() {
    let unknown = check(Method::Get, "/v1/workspaces", &["Bearer other"], READ);
    assert_eq!(unknown, Err(Rejection::UnknownCredential));
    for path in ["/v1/workspaces", "/v1/not-a-route"] {
        let rejection =
            check(Method::Get, path, &["Bearer revoked-token"], READ).expect_err("revoked");
        assert_eq!(rejection.provisional_status(), 401, "{path}");
    }
    let down = guard(
        &request(Method::Get, "/v1/workspaces", BEARER),
        &Down,
        &Unthrottled,
        &org(),
    );
    assert_eq!(down.expect_err("down").provisional_status(), 503);
}

#[test]
fn an_insufficient_scope_is_forbidden() {
    let rejection = check(
        Method::Get,
        "/v1/workspaces/w-1",
        BEARER,
        &[Scope::AuditRead],
    )
    .expect_err("scope missing");
    assert!(matches!(rejection, Rejection::Denied(denial) if denial.check == Check::Scope));
    assert_eq!(rejection.provisional_status(), 403);
}

#[test]
fn unknown_routes_and_uncatalogued_session_routes_are_denied_by_default() {
    let all: Vec<Scope> = Scope::ALL.to_vec();
    for (method, path) in [
        (Method::Get, "/v1/unknown"),
        (Method::Delete, "/v1/workspaces"),
    ] {
        assert_eq!(
            check(method, path, BEARER, &all),
            Err(Rejection::UnknownRoute)
        );
    }
    let session = check(Method::Get, "/v1/sessions/s-1", BEARER, &all).expect_err("held");
    assert!(
        matches!(session, Rejection::Denied(denial) if denial.check == Check::UnknownOperation)
    );
    assert_eq!(session.provisional_status(), 403);
}

#[test]
fn a_workspace_outside_the_binding_is_not_found() {
    let rejection = check(Method::Get, "/v1/workspaces/w-2", BEARER, READ).expect_err("outside");
    assert_eq!(rejection.provisional_status(), 404);
}

#[test]
fn the_throttle_hook_applies_only_after_authentication() {
    let limited = |header: &'static [&'static str], path: &'static str| {
        guard(
            &request(Method::Get, path, header),
            &store(READ),
            &Limited(7),
            &org(),
        )
        .expect_err("refused")
    };
    for path in ["/v1/workspaces", "/v1/not-a-route"] {
        let rejection = limited(BEARER, path);
        assert_eq!(rejection.provisional_status(), 429, "{path}");
        assert_eq!(rejection.retry_after_seconds(), Some(7));
    }
    assert_eq!(limited(&[], "/v1/workspaces").provisional_status(), 401);
}

#[test]
fn token_bytes_never_appear_in_errors_or_debug_output() {
    let header = [BEARER[0]];
    let token = extract_bearer(&header).expect("token");
    assert_eq!(token.secret(), SECRET);
    let guarded = request(Method::Get, "/v1/workspaces/w-1", &header);
    let mut rendered = vec![format!("{token:?}"), format!("{guarded:?}")];
    rendered.push(format!(
        "{:?}",
        guard(&guarded, &store(READ), &Unthrottled, &org())
    ));
    for (path, scopes) in [
        ("/v1/workspaces/w-1", &[][..]),
        ("/v1/x", READ),
        ("/v1/workspaces/w-9", READ),
    ] {
        let outcome = check(Method::Get, path, &header, scopes);
        rendered.push(format!("{outcome:?}"));
    }
    for malformed in [
        "Bearer s3cr3t-Token_value~1 extra",
        "Basic s3cr3t-Token_value~1",
    ] {
        rendered.push(format!("{:?}", extract_bearer(&[malformed])));
    }
    for text in rendered {
        assert!(
            !text.contains(SECRET) && !text.contains("s3cr3t"),
            "leaked: {text}"
        );
    }
}
