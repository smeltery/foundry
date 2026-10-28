//! Injected configuration for a context-consuming backend, not local authentication.
mod profile;
mod profile_types;
mod serve;
mod serve_types;
mod types;
mod update;
mod update_types;
use crate::{Backend, CommandError, Request, Response};
pub use profile_types::*;
use serde_json::Value;
pub use serve_types::*;
pub use types::*;
pub use update::PreSessionExecutor;
pub use update_types::*;

pub struct ConfiguredBackend<I, A, E> {
    inputs: I,
    accounts: A,
    executor: E,
}
impl<I, A, E> ConfiguredBackend<I, A, E> {
    pub fn new(inputs: I, accounts: A, executor: E) -> Self {
        Self {
            inputs,
            accounts,
            executor,
        }
    }
    pub fn into_parts(self) -> (I, A, E) {
        (self.inputs, self.accounts, self.executor)
    }
}
impl<I: Inputs, A: Accounts, E: Executor> Backend for ConfiguredBackend<I, A, E> {
    fn execute(&mut self, mut request: Request) -> Result<Response, CommandError> {
        let scope = scope(&request)?;
        let update_route = update::request_plan(&request)?;
        if scope == Scope::Serve {
            let explicit = serve::take(&mut request)?;
            let InputSnapshot::Serve(inputs) = self
                .inputs
                .snapshot(scope)
                .map_err(ConfigFailure::command_error)?
            else {
                return Err(ConfigFailure::InvalidRequest.command_error());
            };
            let context = ConfigurationContext::Serve(Box::new(serve::resolve(inputs, explicit)));
            return self.executor.execute(request, context);
        }
        // Explicit CLI secrets already existed in Request; never forward them in JSON.
        let explicit = if scope == Scope::Connect {
            Some(take_connect(&mut request).map_err(ConfigFailure::command_error)?)
        } else {
            None
        };
        let profile_changes = if scope == Scope::Connect {
            Some(
                profile::changes(&request.command["arguments"])
                    .map_err(ConfigFailure::command_error)?,
            )
        } else {
            None
        };
        let inputs = self
            .inputs
            .snapshot(scope)
            .map_err(ConfigFailure::command_error)?;
        let context = match (inputs, explicit) {
            (InputSnapshot::Session(inputs), None) => {
                let pre_session = update_route
                    .map(|(route, explicit)| update::plan(route, explicit, inputs.updates));
                let mut endpoints = Vec::new();
                push(
                    &mut endpoints,
                    Origin::EnvironmentEndpoint,
                    inputs.environment_endpoint,
                );
                let endpoint = select(endpoints);
                let mut candidates = Vec::new();
                if let Some(key) = inputs.environment_key {
                    let unusable = key.expose().is_empty();
                    push(&mut candidates, Origin::EnvironmentKey, Some(key));
                    // Empty-key fallback is unresolved; preserve both candidates for the owner.
                    if unusable {
                        push(
                            &mut candidates,
                            Origin::SelectedLogin,
                            inputs.selected_login,
                        );
                        ConfigurationContext::Session {
                            pre_session,
                            identity: Selection::Pending {
                                cause: PendingCause::OwnerPolicy,
                                candidates,
                            },
                            endpoint,
                        }
                    } else {
                        ConfigurationContext::Session {
                            pre_session,
                            identity: self.identity(candidates, &endpoint)?,
                            endpoint,
                        }
                    }
                } else {
                    push(
                        &mut candidates,
                        Origin::SelectedLogin,
                        inputs.selected_login,
                    );
                    ConfigurationContext::Session {
                        pre_session,
                        identity: self.identity(candidates, &endpoint)?,
                        endpoint,
                    }
                }
            }
            (InputSnapshot::Connect(inputs), Some(explicit)) => {
                let mut candidates = Vec::new();
                push(&mut candidates, Origin::ExplicitKey, explicit.key);
                push(
                    &mut candidates,
                    Origin::EnvironmentKey,
                    inputs.environment_key,
                );
                push(&mut candidates, Origin::EnrollmentToken, explicit.token);
                push(
                    &mut candidates,
                    Origin::SavedIdentity,
                    inputs.saved_identity,
                );
                let mut endpoints = Vec::new();
                push(&mut endpoints, Origin::ExplicitEndpoint, explicit.endpoint);
                push(
                    &mut endpoints,
                    Origin::EnvironmentEndpoint,
                    inputs.environment_endpoint,
                );
                push(&mut endpoints, Origin::SavedEndpoint, inputs.saved_endpoint);
                let endpoint = select(endpoints);
                let identity = self.identity(candidates, &endpoint)?;
                let mut proxies = Vec::new();
                push(&mut proxies, Origin::ExplicitProxy, explicit.proxy);
                push(&mut proxies, Origin::SavedProxy, inputs.saved_proxy);
                let profile = profile::resolve(
                    inputs.profile.map(|facts| *facts),
                    profile_changes.ok_or_else(|| ConfigFailure::InvalidRequest.command_error())?,
                    &identity,
                );
                ConfigurationContext::Connect {
                    profile: Box::new(profile),
                    identity,
                    endpoint,
                    proxy: ConnectProxyPolicy {
                        selection: select(proxies),
                    },
                }
            }
            _ => return Err(ConfigFailure::InvalidRequest.command_error()),
        };
        self.executor.execute(request, context)
    }
}
impl<I, A: Accounts, E> ConfiguredBackend<I, A, E> {
    fn identity(
        &mut self,
        candidates: Vec<Candidate>,
        endpoint: &Selection<Candidate>,
    ) -> Result<Selection<Identity>, CommandError> {
        match select(candidates) {
            Selection::Selected(credential)
                if matches!(
                    credential.origin,
                    Origin::ExplicitKey | Origin::EnvironmentKey | Origin::SelectedLogin
                ) =>
            {
                let workspace = self
                    .accounts
                    .workspace(&credential, endpoint)
                    .map_err(ConfigFailure::command_error)?;
                Ok(Selection::Selected(Identity {
                    credential,
                    workspace,
                }))
            }
            Selection::Selected(candidate) => Ok(Selection::Pending {
                cause: PendingCause::OwnerPolicy,
                candidates: vec![candidate],
            }),
            Selection::Pending { cause, candidates } => {
                Ok(Selection::Pending { cause, candidates })
            }
        }
    }
}
fn push(candidates: &mut Vec<Candidate>, origin: Origin, value: Option<PrivateValue>) {
    if let Some(value) = value {
        candidates.push(Candidate { origin, value });
    }
}
fn select(mut candidates: Vec<Candidate>) -> Selection<Candidate> {
    let cause = match candidates.len() {
        0 => PendingCause::Missing,
        1 if !candidates[0].value.expose().is_empty() => {
            return Selection::Selected(candidates.remove(0));
        }
        1 => PendingCause::OwnerPolicy,
        _ => PendingCause::Competing,
    };
    Selection::Pending { cause, candidates }
}
fn scope(request: &Request) -> Result<Scope, CommandError> {
    if request.version != 1 {
        return Err(ConfigFailure::InvalidRequest.command_error());
    }
    require_arguments(&request.command)?;
    if request.command["operation"] == "session" {
        require_arguments(&request.command["arguments"])?;
    }
    if request.command["operation"] == "prompt"
        && !request.command["arguments"]["options"].is_object()
    {
        return Err(ConfigFailure::InvalidRequest.command_error());
    }
    match request.command.get("operation").and_then(Value::as_str) {
        Some("connect") => Ok(Scope::Connect),
        Some("serve") => Ok(Scope::Serve),
        Some("prompt" | "run" | "ls" | "watch" | "send" | "cancel") => Ok(Scope::Session),
        Some("session")
            if matches!(
                request.command["arguments"]["operation"].as_str(),
                Some(
                    "list"
                        | "get"
                        | "start"
                        | "watch"
                        | "send"
                        | "cancel"
                        | "attachments"
                        | "download"
                        | "share_link"
                )
            ) =>
        {
            Ok(Scope::Session)
        }
        _ => Err(CommandError::new(
            "unsupported",
            "Configured backend operation is unavailable",
        )),
    }
}
fn require_arguments(command: &Value) -> Result<(), CommandError> {
    if command.get("arguments").is_some_and(Value::is_object) {
        Ok(())
    } else {
        Err(ConfigFailure::InvalidRequest.command_error())
    }
}
struct ExplicitConnect {
    key: Option<PrivateValue>,
    token: Option<PrivateValue>,
    endpoint: Option<PrivateValue>,
    proxy: Option<PrivateValue>,
}
fn take_connect(request: &mut Request) -> Result<ExplicitConnect, ConfigFailure> {
    let args = request
        .command
        .get_mut("arguments")
        .and_then(Value::as_object_mut)
        .ok_or(ConfigFailure::InvalidRequest)?;
    let mut take = |field: &str| match args.remove(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(PrivateValue::new(value))),
        _ => Err(ConfigFailure::InvalidRequest),
    };
    Ok(ExplicitConnect {
        key: take("api_key")?,
        token: take("token")?,
        endpoint: take("api_url")?,
        proxy: take("proxy")?,
    })
}
