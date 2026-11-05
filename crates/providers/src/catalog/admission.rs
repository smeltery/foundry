use super::{
    AUTO, AdmissionError, AutoCause, CatalogError, CredentialAvailability, Effort, ModelDescriptor,
    ModelId, ModelRef, ProviderCatalog, ProviderId,
};

/// The model a caller asked for. `auto` and an omitted model on a new session
/// both ask for the workspace default; an omitted model on a follow-up keeps the
/// session's model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRequest {
    Omitted,
    Auto,
    Named(ModelId),
}

impl ModelRequest {
    pub fn parse(value: Option<&str>) -> Result<Self, CatalogError> {
        match value {
            None => Ok(Self::Omitted),
            Some(AUTO) => Ok(Self::Auto),
            Some(value) => ModelId::new(value).map(Self::Named),
        }
    }
}

/// The selection a session already runs with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSelection {
    pub model: ModelRef,
    pub effort: Option<Effort>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionRequest {
    /// Narrows a named model lookup; with `auto` or a kept model it must match.
    pub provider_id: Option<ProviderId>,
    pub model: ModelRequest,
    pub effort: Option<Effort>,
    /// The current selection on a follow-up, or `None` for a new session.
    pub session: Option<SessionSelection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionRule {
    WorkspaceDefault,
}

/// How `auto` was asked for. An omitted model on a new session counts as `auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoRequest {
    Token,
    Omitted,
}

/// The record of one `auto` resolution. It is stored on the attempt; `auto`
/// itself never reaches an adapter or a selection identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoResolution {
    pub requested: AutoRequest,
    pub resolved: ModelRef,
    pub catalog_revision: u64,
    pub rule: ResolutionRule,
}

/// How the admitted model was chosen. There is no silent variant: every
/// admission states whether the model was named, kept or resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelChoice {
    Named,
    KeptFromSession,
    Auto(AutoResolution),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortSource {
    Requested,
    KeptFromSession,
    ModelDefault,
    /// The model offers no effort, so none applies.
    NotOffered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffortDecision {
    pub effective: Option<Effort>,
    pub source: EffortSource,
}

/// What admission means for the session's attempts. The runtime applies it; a
/// running attempt's model and effort never change mid-attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptDecision {
    NewSession,
    SameSelection,
    /// A model or effort override starts a new continuation attempt.
    NewAttempt {
        previous: SessionSelection,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    pub model: ModelRef,
    pub choice: ModelChoice,
    pub effort: EffortDecision,
    pub attempt: AttemptDecision,
}

impl ProviderCatalog {
    /// Admits a selection before any execution, or refuses it with a distinct
    /// error. Nothing is resolved later and nothing falls back silently.
    pub fn admit(
        &self,
        request: &SelectionRequest,
        credentials: &impl CredentialAvailability,
    ) -> Result<Admission, AdmissionError> {
        if let Some(provider_id) = &request.provider_id
            && self.provider(provider_id).is_none()
        {
            return Err(AdmissionError::UnknownProvider);
        }
        let (model, choice) = self.choose_model(request, credentials)?;
        let continues_hidden = request
            .session
            .as_ref()
            .is_some_and(|session| session.model == model.key());
        if model.hidden && !continues_hidden {
            return Err(AdmissionError::HiddenModel);
        }
        if !self.model_available(model, credentials) {
            return Err(AdmissionError::ModelUnavailable);
        }
        let effort = choose_effort(model, request)?;
        let attempt = match &request.session {
            None => AttemptDecision::NewSession,
            Some(session) if session.model == model.key() && session.effort == effort.effective => {
                AttemptDecision::SameSelection
            }
            Some(session) => AttemptDecision::NewAttempt {
                previous: session.clone(),
            },
        };
        Ok(Admission {
            model: model.key(),
            choice,
            effort,
            attempt,
        })
    }

    fn choose_model(
        &self,
        request: &SelectionRequest,
        credentials: &impl CredentialAvailability,
    ) -> Result<(&ModelDescriptor, ModelChoice), AdmissionError> {
        let provider_matches = |key: &ModelRef| {
            request
                .provider_id
                .as_ref()
                .is_none_or(|provider_id| *provider_id == key.provider_id)
        };
        let session_model = request.session.as_ref().map(|session| &session.model);
        match (&request.model, &request.session) {
            (ModelRequest::Named(model_id), _) => {
                // A bare ID never matches a hidden model other than the session's,
                // so a retired model cannot make a listed one ambiguous.
                let visible = |model: &ModelDescriptor| {
                    request.provider_id.is_some()
                        || !model.hidden
                        || session_model == Some(&model.key())
                };
                let mut candidates = self.models.values().filter(|model| {
                    model.model_id == *model_id && provider_matches(&model.key()) && visible(model)
                });
                let model = candidates.next().ok_or(AdmissionError::UnknownModel)?;
                if candidates.next().is_some() {
                    return Err(AdmissionError::AmbiguousModel);
                }
                Ok((model, ModelChoice::Named))
            }
            (ModelRequest::Omitted, Some(session)) => {
                if !provider_matches(&session.model) {
                    return Err(AdmissionError::ProviderMismatch);
                }
                let model = self
                    .model(&session.model)
                    .ok_or(AdmissionError::UnknownModel)?;
                Ok((model, ModelChoice::KeptFromSession))
            }
            (ModelRequest::Auto, _) | (ModelRequest::Omitted, None) => {
                let requested = match request.model {
                    ModelRequest::Auto => AutoRequest::Token,
                    _ => AutoRequest::Omitted,
                };
                let unresolvable = AdmissionError::AutoUnresolvable;
                let key = self
                    .workspace_default()
                    .ok_or(unresolvable(AutoCause::NoWorkspaceDefault))?;
                if !provider_matches(key) {
                    return Err(unresolvable(AutoCause::DefaultOutsideProvider));
                }
                // Construction guarantees the default is registered and listed.
                let model = self
                    .model(key)
                    .ok_or(unresolvable(AutoCause::NoWorkspaceDefault))?;
                if !self.model_available(model, credentials) {
                    return Err(unresolvable(AutoCause::DefaultUnavailable));
                }
                let resolution = AutoResolution {
                    requested,
                    resolved: key.clone(),
                    catalog_revision: self.revision(),
                    rule: ResolutionRule::WorkspaceDefault,
                };
                Ok((model, ModelChoice::Auto(resolution)))
            }
        }
    }
}

/// A requested effort must be offered. An omitted effort keeps the session's
/// effort, which the new model must also offer, so a session effort is never
/// dropped silently. A new session, or one whose model took no effort, gets the
/// model's default, or no effort when the model offers none.
fn choose_effort(
    model: &ModelDescriptor,
    request: &SelectionRequest,
) -> Result<EffortDecision, AdmissionError> {
    let decision = |effective, source| Ok(EffortDecision { effective, source });
    if let Some(effort) = request.effort {
        if !model.offers(effort) {
            return Err(AdmissionError::EffortNotOffered);
        }
        return decision(Some(effort), EffortSource::Requested);
    }
    match request.session.as_ref().and_then(|session| session.effort) {
        Some(effort) if model.offers(effort) => {
            decision(Some(effort), EffortSource::KeptFromSession)
        }
        Some(_) => Err(AdmissionError::EffortNotOffered),
        None if model.efforts.is_empty() => decision(None, EffortSource::NotOffered),
        None => decision(model.default_effort, EffortSource::ModelDefault),
    }
}
