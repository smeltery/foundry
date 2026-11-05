//! The guard: extract the bearer, resolve it to a principal, route the request
//! and hand the decision to `authz::authorize`, which owns the check order,
//! scope, role and binding rules.

use std::fmt;

use super::bearer::{BearerError, BearerToken, extract_bearer};
use super::routes::{Method, Route, RouteMatch, route};
use crate::authz::{
    Access, Check, Denial, ErrorClass, MachineId, OperationRequest, Organization, Principal,
    PrincipalId, Role, WorkspaceId, authorize,
};

/// The credential store could not answer; the request is `unavailable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolverUnavailable;

/// Resolves bearer bytes to the principal and credential they identify.
pub trait CredentialResolver {
    /// `Ok(None)` when no credential matches. Revoked or expired credentials
    /// may resolve; the decision refuses them.
    fn resolve(&self, token: &BearerToken) -> Result<Option<Principal>, ResolverUnavailable>;
}

/// The 429 hook. It runs only for authenticated callers, before any route or
/// scope check, and returns the delay the caller must wait.
pub trait Throttle {
    fn retry_after(&self, principal: &Principal, route: Option<&Route>) -> Option<u32>;
}

/// A throttle that never limits.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unthrottled;

impl Throttle for Unthrottled {
    fn retry_after(&self, _principal: &Principal, _route: Option<&Route>) -> Option<u32> {
        None
    }
}

/// What the transport hands the guard. Its `Debug` output omits the header.
#[derive(Clone, Copy)]
pub struct GuardRequest<'a> {
    pub method: Method,
    pub path: &'a str,
    /// Every value of the authorization header, in arrival order.
    pub authorization: &'a [&'a str],
    /// The role a role change assigns, decoded from the body by the caller.
    pub new_role: Option<Role>,
}

impl fmt::Debug for GuardRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GuardRequest")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("authorization", &"<redacted>")
            .field("new_role", &self.new_role)
            .finish()
    }
}

/// An admitted request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission<'p> {
    pub matched: RouteMatch<'p>,
    pub principal: Principal,
    pub access: Access,
}

/// A refused request. No variant carries credential bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Bearer(BearerError),
    /// The bearer matched no credential.
    UnknownCredential,
    ResolverUnavailable,
    /// An authenticated request that matches no route.
    UnknownRoute,
    Denied(Denial),
}

impl Rejection {
    pub fn class(self) -> ErrorClass {
        match self {
            Rejection::Bearer(_) | Rejection::UnknownCredential => ErrorClass::Unauthenticated,
            Rejection::ResolverUnavailable => ErrorClass::Unavailable,
            Rejection::UnknownRoute => ErrorClass::Forbidden,
            Rejection::Denied(denial) => denial.class,
        }
    }

    /// The provisional HTTP status, pending the transport decision.
    pub fn provisional_status(self) -> u16 {
        match self.class() {
            ErrorClass::Unauthenticated => 401,
            ErrorClass::Forbidden => 403,
            ErrorClass::NotFound => 404,
            ErrorClass::Retired => 410,
            ErrorClass::RateLimited { .. } => 429,
            ErrorClass::Unavailable => 503,
        }
    }

    /// The delay a `rate_limited` caller must wait.
    pub fn retry_after_seconds(self) -> Option<u32> {
        match self.class() {
            ErrorClass::RateLimited {
                retry_after_seconds,
            } => Some(retry_after_seconds),
            _ => None,
        }
    }
}

/// No catalog operation has an empty name, so the decision authenticates and
/// throttles an unrouted request and then denies it.
const UNROUTED: &str = "";

/// Guards one request against a fresh snapshot of committed identity state.
pub fn guard<'p>(
    request: &GuardRequest<'p>,
    resolver: &impl CredentialResolver,
    throttle: &impl Throttle,
    org: &Organization,
) -> Result<Admission<'p>, Rejection> {
    let token = extract_bearer(request.authorization).map_err(Rejection::Bearer)?;
    let principal = resolver
        .resolve(&token)
        .map_err(|ResolverUnavailable| Rejection::ResolverUnavailable)?
        .ok_or(Rejection::UnknownCredential)?;
    let matched = route(request.method, request.path);
    let route = matched.as_ref().map(|matched| matched.route);
    let mut operation = match &matched {
        Some(matched) => operation_request(matched, request.new_role),
        None => OperationRequest::new(UNROUTED),
    };
    operation.retry_after_seconds = throttle.retry_after(&principal, route);
    let decision = authorize(Some(&principal), org, &operation);
    match (matched, decision) {
        (Some(matched), Ok(access)) => Ok(Admission {
            matched,
            principal,
            access,
        }),
        (Some(_), Err(denial)) => Err(Rejection::Denied(denial)),
        (None, Err(denial)) if denial.check != Check::UnknownOperation => {
            Err(Rejection::Denied(denial))
        }
        (None, _) => Err(Rejection::UnknownRoute),
    }
}

/// Carries the path parameters the decision reads. Everything else in the
/// path or body stays with the operation handler.
fn operation_request(matched: &RouteMatch<'_>, new_role: Option<Role>) -> OperationRequest {
    let mut operation = OperationRequest::new(matched.route.operation());
    operation.workspace = matched.param("workspace").map(WorkspaceId::new);
    operation.member = matched.param("member").map(PrincipalId::new);
    operation.machine = matched.param("machine").map(MachineId::new);
    operation.single_use = matched.param("single_use").map(WorkspaceId::new);
    operation.new_role = new_role;
    operation
}
