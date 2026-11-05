//! The route table against both specifications: every session API operation
//! and every catalog operation has a route, routes never shadow each other,
//! and anything unrouted resolves to nothing.
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

fn routed() -> BTreeSet<&'static str> {
    ROUTES.iter().map(|route| route.operation()).collect()
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

#[test]
fn every_session_api_operation_has_a_route() {
    let operations = indexed_operations("session-api.md");
    assert!(operations.len() >= 30, "index parse found {operations:?}");
    let missing: Vec<_> = operations
        .iter()
        .filter(|name| name.starts_with("session.") && !routed().contains(name.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "unrouted session operations: {missing:?}"
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
    // The CLI rows of the identity index are not API operations.
    for name in indexed_operations("identity.md") {
        if !name.starts_with("cli.") && !name.starts_with("credential.") {
            assert!(routed().contains(name.as_str()), "unrouted: {name}");
        }
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
            expected.method.name()
        );
        let params = expected.path.matches('{').count();
        assert_eq!(matched.params.len(), params);
    }
    assert_eq!(routed().len(), ROUTES.len());
}

#[test]
fn route_scopes_follow_the_catalog_and_the_session_split() {
    for entry in ROUTES {
        match entry.target {
            Target::Catalog(operation) => assert_eq!(entry.scope(), operation.rule().scope),
            Target::Session { operation, scope } => {
                assert!(
                    Operation::from_name(operation).is_none(),
                    "{operation} is in the catalog"
                );
                let expected = if entry.method == Method::Get {
                    Scope::SessionsRead
                } else {
                    Scope::SessionsRun
                };
                assert_eq!(scope, expected, "{operation}");
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
        (Method::Get, "v1/sessions"),
        (Method::Get, ""),
    ] {
        assert!(
            route(method, path).is_none(),
            "{} {path} resolved",
            method.name()
        );
    }
    let matched = route(Method::Get, "/v1/sessions/s-1/events?after=4").expect("query ignored");
    assert_eq!(matched.route.operation(), "session.events");
    assert_eq!(matched.param("session"), Some("s-1"));
    assert_eq!(Method::from_name("get"), None);
}
