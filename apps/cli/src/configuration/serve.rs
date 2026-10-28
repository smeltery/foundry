use super::{
    Candidate, CommandError, ConfigFailure, Origin, PendingCause, PrivateValue, ProfileCandidate,
    ProfileOrigin, ProfileValue, Request, Selection, push, select, serve_types::*,
};
use serde_json::Value;

pub(super) struct ExplicitServe {
    token: Option<PrivateValue>,
    name: Option<String>,
    directory: Option<String>,
    once: bool,
    service: bool,
    yes: bool,
    sandbox: bool,
}
pub(super) fn take(request: &mut Request) -> Result<ExplicitServe, CommandError> {
    let args = request
        .command
        .get_mut("arguments")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| ConfigFailure::InvalidRequest.command_error())?;
    let stop = boolean(args.get("stop")).map_err(ConfigFailure::command_error)?;
    if stop {
        return Err(CommandError::new(
            "unsupported",
            "Configured worker control is unavailable",
        ));
    }
    let token = match args.remove("token") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(PrivateValue::new(value)),
        _ => return Err(ConfigFailure::InvalidRequest.command_error()),
    };
    let parse = || -> Result<ExplicitServe, ConfigFailure> {
        Ok(ExplicitServe {
            token,
            name: scalar(args.get("name"))?,
            directory: scalar(args.get("dir"))?,
            once: boolean(args.get("once"))?,
            service: boolean(args.get("service"))?,
            yes: boolean(args.get("yes"))?,
            sandbox: boolean(args.get("sandbox"))?,
        })
    };
    parse().map_err(ConfigFailure::command_error)
}
fn scalar(value: Option<&Value>) -> Result<Option<String>, ConfigFailure> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(ConfigFailure::InvalidRequest),
    }
}
fn boolean(value: Option<&Value>) -> Result<bool, ConfigFailure> {
    match value {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(ConfigFailure::InvalidRequest),
    }
}
pub(super) fn resolve(inputs: ServeInputs, explicit: ExplicitServe) -> ServeContext {
    let mut candidates = Vec::new();
    push(&mut candidates, Origin::ExplicitToken, explicit.token);
    push(
        &mut candidates,
        Origin::EnvironmentToken,
        inputs.environment_token,
    );
    push(
        &mut candidates,
        Origin::SavedWorkerIdentity,
        inputs.saved_worker_identity,
    );
    push(
        &mut candidates,
        Origin::SelectedLogin,
        inputs.selected_login,
    );
    let name_token_conflict = explicit.name.is_some() && candidates.iter().any(is_token);
    let selection = select(candidates);
    let identity = match selection {
        Selection::Selected(candidate) if name_token_conflict => Selection::Pending {
            cause: PendingCause::OwnerPolicy,
            candidates: vec![candidate],
        },
        Selection::Selected(candidate) => Selection::Selected(match candidate.origin {
            Origin::ExplicitToken | Origin::EnvironmentToken => {
                ServeIdentity::TokenBound(candidate)
            }
            Origin::SavedWorkerIdentity => ServeIdentity::SavedWorker(candidate),
            _ => ServeIdentity::SelectedLogin(candidate),
        }),
        Selection::Pending { cause, candidates } => Selection::Pending { cause, candidates },
    };
    let machine = match &identity {
        Selection::Selected(ServeIdentity::TokenBound(_)) => ServeMachine::TokenBound,
        Selection::Selected(ServeIdentity::SavedWorker(_)) if explicit.name.is_none() => {
            ServeMachine::SavedWorker
        }
        Selection::Selected(ServeIdentity::SelectedLogin(_)) => ServeMachine::Named(value(
            explicit.name,
            inputs.hostname,
            ProfileOrigin::Hostname,
        )),
        _ => ServeMachine::Pending {
            name: unresolved(value(explicit.name, None, ProfileOrigin::Hostname)),
        },
    };
    let persistence = if explicit.once {
        ServePersistence::NoSave
    } else if matches!(
        &identity,
        Selection::Selected(ServeIdentity::SavedWorker(_))
    ) {
        ServePersistence::ReuseSavedWorkerIdentity
    } else {
        ServePersistence::RetainVerifiedWorkerIdentityAfterSuccessfulJoin
    };
    ServeContext {
        pre_session: super::update::plan(super::UpdateRoute::Serve, false, inputs.updates),
        identity,
        machine,
        directory: value(explicit.directory, inputs.cwd, ProfileOrigin::Cwd),
        persistence,
        execution: if explicit.service {
            ServeExecution::ForegroundJoinThenBackground
        } else {
            ServeExecution::Foreground
        },
        yes: explicit.yes,
        sandbox: explicit.sandbox,
    }
}
fn is_token(candidate: &Candidate) -> bool {
    matches!(
        candidate.origin,
        Origin::ExplicitToken | Origin::EnvironmentToken
    )
}
fn value(
    explicit: Option<String>,
    default: Option<String>,
    origin: ProfileOrigin,
) -> ProfileValue<String> {
    if explicit.as_ref().is_some_and(String::is_empty) {
        let mut candidates = vec![ProfileCandidate {
            value: String::new(),
            origin: ProfileOrigin::Explicit,
        }];
        if let Some(value) = default {
            candidates.push(ProfileCandidate { value, origin });
        }
        return ProfileValue::Pending {
            cause: PendingCause::OwnerPolicy,
            candidates,
        };
    }
    let chosen = explicit
        .map(|value| ProfileCandidate {
            value,
            origin: ProfileOrigin::Explicit,
        })
        .or_else(|| default.map(|value| ProfileCandidate { value, origin }));
    match chosen {
        Some(candidate) if !candidate.value.is_empty() => ProfileValue::Selected(candidate),
        Some(candidate) => ProfileValue::Pending {
            cause: PendingCause::OwnerPolicy,
            candidates: vec![candidate],
        },
        None => ProfileValue::Pending {
            cause: PendingCause::Missing,
            candidates: Vec::new(),
        },
    }
}
fn unresolved(value: ProfileValue<String>) -> ProfileValue<String> {
    match value {
        ProfileValue::Selected(candidate) => ProfileValue::Pending {
            cause: PendingCause::OwnerPolicy,
            candidates: vec![candidate],
        },
        pending => pending,
    }
}
