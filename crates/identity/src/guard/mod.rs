//! The protected-route guard at library level: bearer extraction, the
//! per-route scope table, and the `unauthenticated`, `forbidden` and
//! `rate_limited` outcomes, as specified in `docs/architecture/identity.md`.
//! It adds no HTTP server; binding it to a transport waits on NIC-36 C2. All
//! wire names, including paths and methods, are provisional.

mod bearer;
mod check;
mod routes;

pub use bearer::{BearerError, BearerToken, extract_bearer};
pub use check::{
    Admission, CredentialResolver, GuardRequest, Rejection, ResolverUnavailable, Throttle,
    Unthrottled, guard,
};
pub use routes::{Method, ROUTES, Route, RouteMatch, Target, route};
