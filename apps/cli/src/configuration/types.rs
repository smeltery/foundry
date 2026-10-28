use crate::{CommandError, Request, Response};

/// Private input bytes: intentionally neither printable nor serializable.
pub struct PrivateValue(String);
impl PrivateValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    /// Access is restricted by ownership of the injected backend/executor.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Session,
    Connect,
    Serve,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    ExplicitKey,
    EnvironmentKey,
    SelectedLogin,
    EnrollmentToken,
    ExplicitToken,
    EnvironmentToken,
    SavedWorkerIdentity,
    SavedIdentity,
    ExplicitEndpoint,
    EnvironmentEndpoint,
    SavedEndpoint,
    ExplicitProxy,
    SavedProxy,
}
pub struct Candidate {
    pub origin: Origin,
    pub value: PrivateValue,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingCause {
    Missing,
    Competing,
    OwnerPolicy,
}
pub enum Selection<T> {
    Selected(T),
    Pending {
        cause: PendingCause,
        candidates: Vec<Candidate>,
    },
}

/// A workspace identifier supplied by the account interface, not inferred from key bytes.
pub struct Workspace(String);
impl Workspace {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn id(&self) -> &str {
        &self.0
    }
}
pub struct Identity {
    pub credential: Candidate,
    pub workspace: Workspace,
}
#[derive(Default)]
pub struct SessionInputs {
    pub updates: super::UpdateInputs,
    pub environment_endpoint: Option<PrivateValue>,
    pub environment_key: Option<PrivateValue>,
    pub selected_login: Option<PrivateValue>,
}
#[derive(Default)]
pub struct ConnectInputs {
    pub profile: Option<Box<super::ProfileFacts>>,
    pub environment_key: Option<PrivateValue>,
    pub saved_identity: Option<PrivateValue>,
    pub environment_endpoint: Option<PrivateValue>,
    pub saved_endpoint: Option<PrivateValue>,
    pub saved_proxy: Option<PrivateValue>,
    /// Observed only for exclusion; never forwarded or selected.
    pub https_proxy: Option<PrivateValue>,
}
pub enum InputSnapshot {
    Session(SessionInputs),
    Connect(ConnectInputs),
    Serve(super::ServeInputs),
}

/// Value-free failures prevent injected secret-bearing diagnostics from escaping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigFailure {
    InputsUnavailable,
    AccountUnavailable,
    InvalidRequest,
}
impl ConfigFailure {
    pub(super) fn command_error(self) -> CommandError {
        let message = match self {
            Self::InputsUnavailable => "Configuration inputs unavailable",
            Self::AccountUnavailable => "Account resolution unavailable",
            Self::InvalidRequest => "Invalid configured backend request",
        };
        CommandError::new("configuration", message)
    }
}
pub trait Inputs {
    fn snapshot(&mut self, scope: Scope) -> Result<InputSnapshot, ConfigFailure>;
}
pub trait Accounts {
    fn workspace(
        &mut self,
        credential: &Candidate,
        endpoint: &Selection<Candidate>,
    ) -> Result<Workspace, ConfigFailure>;
}
pub struct ConnectProxyPolicy {
    pub selection: Selection<Candidate>,
}
impl ConnectProxyPolicy {
    /// An executor must not interpret an unspecified proxy as HTTPS_PROXY fallback.
    pub fn excludes_https_proxy(&self) -> bool {
        true
    }
}
pub enum ConfigurationContext {
    Serve(Box<super::ServeContext>),
    Session {
        pre_session: Option<super::PreSessionPlan>,
        identity: Selection<Identity>,
        endpoint: Selection<Candidate>,
    },
    Connect {
        profile: Box<super::ConnectProfile>,
        identity: Selection<Identity>,
        endpoint: Selection<Candidate>,
        proxy: ConnectProxyPolicy,
    },
}
/// The executor owns unresolved policy decisions. No default discards the context.
pub trait Executor {
    fn execute(
        &mut self,
        request: Request,
        context: ConfigurationContext,
    ) -> Result<Response, CommandError>;
}
