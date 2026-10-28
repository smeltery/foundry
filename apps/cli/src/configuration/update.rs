use super::{
    CommandError, ConfigFailure, ConfigurationContext, Executor, PrivateValue, Request, Response,
    update_types::*,
};
use serde_json::Value;

pub struct PreSessionExecutor<O, S, L> {
    owner: O,
    sink: S,
    launcher: L,
}
impl<O, S, L> PreSessionExecutor<O, S, L> {
    pub fn new(owner: O, sink: S, launcher: L) -> Self {
        Self {
            owner,
            sink,
            launcher,
        }
    }
    pub fn into_parts(self) -> (O, S, L) {
        (self.owner, self.sink, self.launcher)
    }
}
impl<O: PreSessionOwner, S: UpdateSink, L: UpdateLauncher> Executor
    for PreSessionExecutor<O, S, L>
{
    fn execute(
        &mut self,
        request: Request,
        context: ConfigurationContext,
    ) -> Result<Response, CommandError> {
        let route = request_plan(&request)?;
        let plan = match &context {
            ConfigurationContext::Session { pre_session, .. } => pre_session.as_ref(),
            ConfigurationContext::Serve(context) => Some(&context.pre_session),
            ConfigurationContext::Connect { .. } => None,
        };
        let plan = match (route, plan) {
            (None, None) => return self.launcher.launch_installed(request, context),
            (Some((route, explicit)), Some(plan))
                if plan.route == route && plan.explicit_update == explicit =>
            {
                plan
            }
            _ => return Err(UpdateFailure::InvalidPlan.command_error()),
        };
        let preparation = match self.owner.prepare(plan) {
            Ok(preparation) => preparation,
            Err(failure) => {
                self.sink
                    .emit(UpdateDiagnostic::Error(failure))
                    .map_err(UpdateFailure::command_error)?;
                return Err(failure.command_error());
            }
        };
        if preparation.daily_notice
            && preparation.notice == NoticeDecision::Allow
            && !plan.daily_notice_suppressed()
        {
            self.sink
                .emit(UpdateDiagnostic::DailyUpdateNotice)
                .map_err(UpdateFailure::command_error)?;
        }
        match preparation.outcome {
            PreparationOutcome::Installed => self.launcher.launch_installed(request, context),
            PreparationOutcome::Updated(handoff) => {
                self.launcher.launch_updated(handoff, request, context)
            }
            PreparationOutcome::ReleaseServerUnavailable => {
                self.sink
                    .emit(UpdateDiagnostic::PreUpdateWarning)
                    .map_err(UpdateFailure::command_error)?;
                self.launcher.launch_installed(request, context)
            }
        }
    }
}
pub(super) fn request_plan(request: &Request) -> Result<Option<(UpdateRoute, bool)>, CommandError> {
    match request.command["operation"].as_str() {
        Some("prompt") => {
            let options = request.command["arguments"]["options"]
                .as_object()
                .ok_or_else(|| ConfigFailure::InvalidRequest.command_error())?;
            let explicit = match options.get("update") {
                None => false,
                Some(Value::Bool(value)) => *value,
                _ => return Err(ConfigFailure::InvalidRequest.command_error()),
            };
            Ok(Some((UpdateRoute::Prompt, explicit)))
        }
        Some("run") => Ok(Some((UpdateRoute::Run, false))),
        Some("serve") => Ok(Some((UpdateRoute::Serve, false))),
        _ => Ok(None),
    }
}
pub(super) fn plan(
    route: UpdateRoute,
    explicit_update: bool,
    inputs: UpdateInputs,
) -> PreSessionPlan {
    PreSessionPlan {
        route,
        explicit_update,
        auto_update: observation(inputs.auto_update),
        no_update_notice: observation(inputs.no_update_notice),
    }
}
fn observation(value: Option<PrivateValue>) -> SettingObservation {
    match value {
        None => SettingObservation::Absent,
        Some(value) if value.expose() == "1" => SettingObservation::ExactOne,
        Some(value) => SettingObservation::Unresolved(value),
    }
}
