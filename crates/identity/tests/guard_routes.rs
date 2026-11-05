//! The route table against both specifications: the routed operations are
//! exactly the API operations of the two coverage indexes, routes never
//! shadow each other, and anything unrouted or non-canonical resolves to
//! nothing.
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use foundry_identity::authz::{Operation, Scope};
use foundry_identity::guard::{Method, ROUTES, Target, route};

/// Backticked operation names in a specification's coverage index.
fn indexed_operations(document: &str) -> BTreeSet<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/architecture")
        .join(document);
    let text = fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
    let index = text
        .split("## Coverage index")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .unwrap_or_else(|| panic!("{document} has no coverage index"));
    index
        .lines()
        .filter_map(|line| line.split('|').nth(2))
        .flat_map(|cell| cell.split('`').skip(1).step_by(2))
        .map(str::to_owned)
        .collect()
}

fn routed() -> BTreeSet<String> {
    ROUTES
        .iter()
        .map(|route| route.operation().to_owned())
        .collect()
}

/// A concrete path for a template, with each parameter replaced by a sample.
fn sample(template: &str) -> String {
    template
        .split('/')
        .map(|segment| {
            if segment.starts_with('{') {
                "x-1"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn label(method: Method) -> &'static str {
    method.name().unwrap_or("OTHER")
}

#[test]
fn routed_operations_equal_the_specified_api_operations() {
    let session = indexed_operations("session-api.md");
    assert!(session.len() >= 30, "index parse found {session:?}");
    // The CLI and credential rows of the identity index are not API operations.
    let specified: BTreeSet<String> = session
        .into_iter()
        .chain(indexed_operations("identity.md"))
        .filter(|name| !name.starts_with("cli.") && !name.starts_with("credential."))
        .collect();
    let routed = routed();
    let unrouted: Vec<_> = specified.difference(&routed).collect();
    let unspecified: Vec<_> = routed.difference(&specified).collect();
    assert!(unrouted.is_empty(), "specified but unrouted: {unrouted:?}");
    assert!(
        unspecified.is_empty(),
        "routed but unspecified: {unspecified:?}"
    );
}

#[test]
fn every_catalog_operation_has_exactly_one_route() {
    for operation in Operation::ALL {
        let count = ROUTES
            .iter()
            .filter(|route| route.target == Target::Catalog(*operation))
            .count();
        assert_eq!(count, 1, "{} has {count} routes", operation.name());
    }
}

#[test]
fn every_route_resolves_to_itself_and_no_operation_is_routed_twice() {
    for expected in ROUTES {
        let path = sample(expected.path);
        let matched = route(expected.method, &path).expect("route resolves");
        assert_eq!(
            matched.route,
            expected,
            "{} {path} is shadowed",
            label(expected.method)
        );
        let params = expected.path.matches('{').count();
        assert_eq!(matched.params.len(), params);
    }
    assert_eq!(routed().len(), ROUTES.len());
}

#[test]
fn only_catalog_routes_enforce_a_scope_and_session_scopes_are_placeholders() {
    for entry in ROUTES {
        match entry.target {
            Target::Catalog(operation) => {
                assert_eq!(entry.enforced_scope(), Some(operation.rule().scope));
            }
            // Session placeholders come from no specification and are not
            // enforced: the decision denies these routes until the catalog
            // gains them. The test pins only that, not any scope rule.
            Target::Session {
                operation,
                placeholder_scope,
            } => {
                assert!(
                    Operation::from_name(operation).is_none(),
                    "{operation} is catalogued"
                );
                assert_eq!(entry.enforced_scope(), None, "{operation}");
                assert!(
                    matches!(placeholder_scope, Scope::SessionsRead | Scope::SessionsRun),
                    "{operation}"
                );
            }
        }
    }
}

#[test]
fn unrouted_requests_resolve_to_nothing() {
    for (method, path) in [
        (Method::Get, "/v1/unknown"),
        (Method::Get, "/v1/workspaces/"),
        (Method::Get, "/v1/workspaces//members"),
        (Method::Get, "/v1/sessions/s-1/extra/segment"),
        (Method::Patch, "/v1/sessions"),
        (Method::Other, "/v1/sessions"),
        (Method::Get, "v1/sessions"),
        (Method::Get, ""),
    ] {
        assert!(
            route(method, path).is_none(),
            "{} {path} resolved",
            label(method)
        );
    }
    let matched = route(Method::Get, "/v1/sessions/s-1/events?after=4").expect("query ignored");
    assert_eq!(matched.route.operation(), "session.events");
    assert_eq!(matched.param("session"), Some("s-1"));
    for name in ["get", "HEAD", "OPTIONS", "TRACE", ""] {
        assert_eq!(Method::from_name(name), Method::Other, "{name}");
    }
    assert_eq!(Method::from_name("DELETE"), Method::Delete);
}

#[test]
fn non_canonical_parameters_never_route() {
    let base = "/v1/workspaces/w-1/members/";
    for member in [
        "owner%2D1",
        "%6Fwner-1",
        "OWNER-1",
        "Owner-1",
        "..",
        ".",
        "owner-1%2F..",
        "owner.1",
        "owner 1",
        "owner+1",
    ] {
        for (method, suffix) in [(Method::Delete, ""), (Method::Put, "/role")] {
            let path = format!("{base}{member}{suffix}");
            assert!(
                route(method, &path).is_none(),
                "{} {path} routed",
                label(method)
            );
        }
    }
    assert!(route(Method::Get, "/v1/workspaces/W-1").is_none());
    assert!(route(Method::Delete, "/v1/workspaces/w-1/members/owner_1-2").is_some());
}
