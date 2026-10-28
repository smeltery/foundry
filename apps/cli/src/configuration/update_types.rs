use super::{CommandError, ConfigurationContext, PrivateValue, Request, Response};

#[derive(Default)]
pub struct UpdateInputs {
    pub auto_update: Option<PrivateValue>,
    pub no_update_notice: Option<PrivateValue>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateRoute {
    Prompt,
    Run,
    Serve,
}
pub enum SettingObservation {
    Absent,
    ExactOne,
    Unresolved(PrivateValue),
}
pub struct PreSessionPlan {
    pub route: UpdateRoute,
    pub explicit_update: bool,
    pub auto_update: SettingObservation,
    pub no_update_notice: SettingObservation,
}
impl PreSessionPlan {
    pub fn update_requested(&self) -> bool {
        self.explicit_update || matches!(self.auto_update, SettingObservation::ExactOne)
    }
    pub fn daily_notice_suppressed(&self) -> bool {
        matches!(self.no_update_notice, SettingObservation::ExactOne)
    }
}
/// Opaque launch authority supplied by the owner; construction does not verify a version.
pub struct UpdatedLaunch(PrivateValue);
impl UpdatedLaunch {
    pub fn new(handle: PrivateValue) -> Self {
        Self(handle)
    }
    pub fn handle(&self) -> &PrivateValue {
        &self.0
    }
}
pub struct Preparation {
    pub outcome: PreparationOutcome,
    pub daily_notice: bool,
    pub notice: NoticeDecision,
}
pub enum PreparationOutcome {
    Installed,
    Updated(UpdatedLaunch),
    ReleaseServerUnavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeDecision {
    Allow,
    Suppress,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateFailure {
    PolicyUnresolved,
    Unavailable,
    Verification,
    Other,
    SinkUnavailable,
    InvalidPlan,
}
impl UpdateFailure {
    pub(super) fn command_error(self) -> CommandError {
        let message = match self {
            Self::PolicyUnresolved => "Pre-session policy is unresolved",
            Self::Unavailable => "Pre-session preparation is unavailable",
            Self::Verification => "Pre-session verification failed",
            Self::Other => "Pre-session preparation failed",
            Self::SinkUnavailable => "Pre-session diagnostic delivery failed",
            Self::InvalidPlan => "Invalid pre-session plan",
        };
        CommandError::new("configuration", message)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateDiagnostic {
    DailyUpdateNotice,
    PreUpdateWarning,
    Error(UpdateFailure),
}
pub trait PreSessionOwner {
    fn prepare(&mut self, plan: &PreSessionPlan) -> Result<Preparation, UpdateFailure>;
}
pub trait UpdateSink {
    fn emit(&mut self, diagnostic: UpdateDiagnostic) -> Result<(), UpdateFailure>;
}
pub trait UpdateLauncher {
    fn launch_installed(
        &mut self,
        request: Request,
        context: ConfigurationContext,
    ) -> Result<Response, CommandError>;
    fn launch_updated(
        &mut self,
        handoff: UpdatedLaunch,
        request: Request,
        context: ConfigurationContext,
    ) -> Result<Response, CommandError>;
}
