//! The protected-route table: each provisional method and path template and
//! the operation it invokes. Paths, methods, segment names and the parameter
//! alphabet are provisional pending NIC-34 D1/D6 and NIC-36 C2.

use crate::authz::{Operation, Operation as Op, Scope};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    /// Any other method, such as `HEAD` or `OPTIONS`. No route uses it, so
    /// the guard denies it as an unknown route.
    Other,
}

impl Method {
    /// The routable methods.
    pub const ALL: [Method; 5] = [
        Method::Get,
        Method::Post,
        Method::Put,
        Method::Patch,
        Method::Delete,
    ];

    /// The wire name; `Other` has none.
    pub fn name(self) -> Option<&'static str> {
        match self {
            Method::Get => Some("GET"),
            Method::Post => Some("POST"),
            Method::Put => Some("PUT"),
            Method::Patch => Some("PATCH"),
            Method::Delete => Some("DELETE"),
            Method::Other => None,
        }
    }

    /// Method names are case-sensitive; anything unrecognized is `Other`.
    pub fn from_name(name: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|method| method.name() == Some(name))
            .unwrap_or(Method::Other)
    }
}

/// What a route invokes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// An operation of the authorization catalog, which owns and enforces its
    /// scope.
    Catalog(Operation),
    /// A session API operation. The catalog has no session operations yet, so
    /// the decision denies every session route by default.
    Session {
        operation: &'static str,
        /// A provisional placeholder that is not enforced and comes from no
        /// specification: the session API assigns no scope per operation.
        /// The catalog decides the real scope when session operations join it.
        placeholder_scope: Scope,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub method: Method,
    /// Segments are literals or `{name}` parameters. The parameters
    /// `workspace`, `member`, `machine` and `single_use` feed the decision.
    pub path: &'static str,
    pub target: Target,
}

impl Route {
    pub fn operation(&self) -> &'static str {
        match self.target {
            Target::Catalog(operation) => operation.name(),
            Target::Session { operation, .. } => operation,
        }
    }

    /// The scope the catalog enforces, where `Some(None)` admits any valid
    /// credential. Session routes have no enforced scope yet.
    pub fn enforced_scope(&self) -> Option<Option<Scope>> {
        match self.target {
            Target::Catalog(operation) => Some(operation.rule().scope),
            Target::Session { .. } => None,
        }
    }

    /// Matches a concrete path and returns its parameters in template order.
    /// Only canonical parameters match; see `is_canonical_param`.
    fn matches<'p>(&self, path: &'p str) -> Option<Vec<(&'static str, &'p str)>> {
        let mut template = self.path.split('/');
        let mut concrete = path.split('/');
        let mut params = Vec::new();
        loop {
            match (template.next(), concrete.next()) {
                (None, None) => return Some(params),
                (Some(expected), Some(actual)) => {
                    match expected.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                        Some(name) if is_canonical_param(actual) => params.push((name, actual)),
                        None if expected == actual => {}
                        _ => return None,
                    }
                }
                _ => return None,
            }
        }
    }
}

/// The one canonical spelling of a path parameter: lowercase ASCII letters,
/// digits, `-` and `_`. The guard never decodes, so a percent-encoded,
/// dot-segment or case-variant spelling cannot match a route and reach a
/// handler that might read a different identifier than the decision did.
/// The specification defines no identifier alphabet; this one is provisional.
fn is_canonical_param(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'))
}

/// A matched route and its canonical path parameters. Handlers use them as
/// given and never decode them again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteMatch<'p> {
    pub route: &'static Route,
    pub params: Vec<(&'static str, &'p str)>,
}

impl RouteMatch<'_> {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find_map(|(key, value)| (*key == name).then_some(*value))
    }
}

/// Resolves a request line. The query string is ignored; anything else that
/// matches no route resolves to nothing and is denied by default.
pub fn route(method: Method, path: &str) -> Option<RouteMatch<'_>> {
    let path = path.split_once('?').map_or(path, |(path, _query)| path);
    ROUTES
        .iter()
        .filter(|route| route.method == method)
        .find_map(|route| {
            let params = route.matches(path)?;
            Some(RouteMatch { route, params })
        })
}

macro_rules! routes {
    ($($method:ident $path:literal => $target:expr;)+) => {
        /// Every protected route. No two routes match the same request.
        pub const ROUTES: &[Route] = &[$(Route {
            method: Method::$method,
            path: $path,
            target: $target,
        },)+];
    };
}

// Placeholder split for session routes: reads take `sessions.read` and every
// other route `sessions.run`. Not enforced; see `Target::Session`.
const fn read(operation: &'static str) -> Target {
    Target::Session {
        operation,
        placeholder_scope: Scope::SessionsRead,
    }
}

const fn run(operation: &'static str) -> Target {
    Target::Session {
        operation,
        placeholder_scope: Scope::SessionsRun,
    }
}

const fn op(operation: Operation) -> Target {
    Target::Catalog(operation)
}

routes! {
    Get "/v1/identity" => op(Op::IdentitySelf);
    Get "/v1/deployment" => op(Op::ServiceDeployment);
    Get "/v1/api-description" => op(Op::ServiceApiDescription);
    Post "/v1/feedback" => op(Op::FeedbackSubmit);
    Get "/v1/workspaces" => op(Op::WorkspaceList);
    Post "/v1/workspaces" => op(Op::WorkspaceCreate);
    Get "/v1/workspaces/{workspace}" => op(Op::WorkspaceGet);
    Patch "/v1/workspaces/{workspace}" => op(Op::WorkspaceRename);
    Delete "/v1/workspaces/{workspace}" => op(Op::WorkspaceDelete);
    Get "/v1/workspaces/{workspace}/audit" => op(Op::AuditList);
    Post "/v1/workspaces/{workspace}/single-use" => op(Op::SingleUseCreate);
    Delete "/v1/workspaces/{workspace}/single-use/{single_use}" => op(Op::SingleUseRetire);
    Get "/v1/workspaces/{workspace}/members" => op(Op::MemberList);
    Post "/v1/workspaces/{workspace}/members" => op(Op::MemberInvite);
    Get "/v1/workspaces/{workspace}/members/{member}" => op(Op::MemberGet);
    Delete "/v1/workspaces/{workspace}/members/{member}" => op(Op::MemberRemove);
    Put "/v1/workspaces/{workspace}/members/{member}/role" => op(Op::MemberSetRole);
    Get "/v1/workspaces/{workspace}/network-policy" => op(Op::NetworkPolicyGet);
    Put "/v1/workspaces/{workspace}/network-policy" => op(Op::NetworkPolicyReplace);
    Post "/v1/workspaces/{workspace}/secret-import/handoff" => op(Op::SecretImportHandoff);
    Post "/v1/workspaces/{workspace}/secret-import/decision" => op(Op::SecretImportDecision);
    Post "/v1/workspaces/{workspace}/secret-import/complete" => op(Op::SecretImportComplete);
    Post "/v1/workspaces/{workspace}/pairing/claim" => op(Op::PairingClaim);
    Post "/v1/workspaces/{workspace}/pairing/fail" => op(Op::PairingFail);
    Get "/v1/workspaces/{workspace}/tiers" => op(Op::TierList);
    Put "/v1/workspaces/{workspace}/tier" => op(Op::TierSet);
    Get "/v1/workspaces/{workspace}/tier" => op(Op::TierGet);
    Get "/v1/workspaces/{workspace}/role" => op(Op::RoleOwn);
    Get "/v1/workspaces/{workspace}/settings" => op(Op::SettingsList);
    Get "/v1/workspaces/{workspace}/settings/{setting}" => op(Op::SettingsGet);
    Put "/v1/workspaces/{workspace}/settings/{setting}" => op(Op::SettingsReplace);
    Post "/v1/sessions" => run("session.create");
    Post "/v1/prestarts" => run("session.prestart");
    Get "/v1/sessions" => read("session.list");
    Get "/v1/sessions/{session}" => read("session.get");
    Delete "/v1/sessions/{session}" => run("session.delete");
    Get "/v1/sessions/{session}/events" => read("session.events");
    Get "/v1/sessions/{session}/messages" => read("session.messages");
    Post "/v1/sessions/{session}/messages" => run("session.send");
    Delete "/v1/sessions/{session}/messages/{message}" => run("session.withdraw");
    Post "/v1/sessions/{session}/messages/{message}/steer" => run("session.steer");
    Get "/v1/sessions/{session}/conversation" => read("session.conversation");
    Post "/v1/sessions/{session}/cancel" => run("session.cancel");
    Post "/v1/sessions/{session}/retry" => run("session.retry");
    Post "/v1/sessions/{session}/fork" => run("session.fork");
    Post "/v1/sessions/{session}/rewind" => run("session.rewind");
    Post "/v1/sessions/{session}/compact" => run("session.compact");
    Post "/v1/sessions/{session}/archive" => run("session.archive");
    Post "/v1/sessions/{session}/unarchive" => run("session.unarchive");
    Put "/v1/sessions/{session}/title" => run("session.rename");
    Get "/v1/sessions/{session}/tags" => read("session.tags.list");
    Post "/v1/sessions/{session}/tags" => run("session.tags.append");
    Put "/v1/sessions/{session}/tags" => run("session.tags.replace");
    Put "/v1/sessions/{session}/project" => run("session.move_project");
    Get "/v1/sessions/{session}/attachments" => read("session.attachments.list");
    Get "/v1/sessions/{session}/attachments/{attachment}" => read("session.attachments.get");
    Get "/v1/sessions/{session}/links" => read("session.links.list");
    Post "/v1/sessions/{session}/links" => run("session.links.create");
    Delete "/v1/sessions/{session}/links/{link}" => run("session.links.revoke");
    Get "/v1/insights" => read("session.list_insights");
    Get "/v1/sessions/{session}/insight" => read("session.insight.get");
    Post "/v1/sessions/{session}/insight" => run("session.insight.generate");
    Get "/v1/sessions/{session}/trajectory" => read("session.trajectory");
}
