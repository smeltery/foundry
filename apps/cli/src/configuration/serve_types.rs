use super::{Candidate, PrivateValue, ProfileValue, Selection};

#[derive(Default)]
pub struct ServeInputs {
    pub updates: super::UpdateInputs,
    pub environment_token: Option<PrivateValue>,
    pub saved_worker_identity: Option<PrivateValue>,
    pub selected_login: Option<PrivateValue>,
    pub cwd: Option<String>,
    pub hostname: Option<String>,
}
/// Supplied identity material, not proof of enrollment or authentication.
pub enum ServeIdentity {
    TokenBound(Candidate),
    SavedWorker(Candidate),
    SelectedLogin(Candidate),
}
pub enum ServeMachine {
    Named(ProfileValue<String>),
    TokenBound,
    SavedWorker,
    Pending { name: ProfileValue<String> },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServePersistence {
    NoSave,
    /// After a successful join supplies verified worker identity; never the input token.
    RetainVerifiedWorkerIdentityAfterSuccessfulJoin,
    /// Reconnect using the selected saved identity without replacing it.
    ReuseSavedWorkerIdentity,
}
impl ServePersistence {
    pub fn permits_token_persistence(&self) -> bool {
        false
    }
    pub fn preserves_unrelated_saved_state(&self) -> bool {
        true
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeExecution {
    Foreground,
    ForegroundJoinThenBackground,
}
pub struct ServeContext {
    pub pre_session: super::PreSessionPlan,
    pub identity: Selection<ServeIdentity>,
    pub machine: ServeMachine,
    pub directory: ProfileValue<String>,
    pub persistence: ServePersistence,
    pub execution: ServeExecution,
    pub yes: bool,
    pub sandbox: bool,
}
