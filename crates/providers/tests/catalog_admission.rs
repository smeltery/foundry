use foundry_providers::catalog::{
    AdmissionError, AttemptDecision, AuthKind, AutoCause, AutoRequest, AutoResolution,
    CatalogError, ConfiguredCredentials, Effort, EffortDecision, EffortSource, LoginUnavailable,
    ModelChoice, ModelDescriptor, ModelId, ModelRef, ModelRequest, OauthLogin, ProviderCatalog,
    ProviderDescriptor, ProviderId, ProviderKind, ResolutionRule, SelectionRequest,
    SessionSelection,
};

fn id(value: &str) -> ProviderId {
    ProviderId::new(value).unwrap()
}

fn key(provider_id: &str, model_id: &str) -> ModelRef {
    ModelRef {
        provider_id: id(provider_id),
        model_id: ModelId::new(model_id).unwrap(),
    }
}

fn provider(provider_id: &str, auth_kind: AuthKind) -> ProviderDescriptor {
    ProviderDescriptor {
        provider_id: id(provider_id),
        kind: match auth_kind {
            AuthKind::None => ProviderKind::Fixture,
            _ => ProviderKind::HostedApi,
        },
        auth_kinds: vec![auth_kind],
        config_fields: vec![],
        oauth_login: OauthLogin::Unavailable(LoginUnavailable::NotSupported),
        efforts: Effort::ALL.to_vec(),
        endpoint_format: None,
    }
}

fn model(provider_id: &str, model_id: &str, efforts: &[Effort], hidden: bool) -> ModelDescriptor {
    ModelDescriptor {
        provider_id: id(provider_id),
        model_id: ModelId::new(model_id).unwrap(),
        label: format!("Model {model_id}"),
        efforts: efforts.to_vec(),
        default_effort: efforts.first().copied(),
        hidden,
    }
}

/// `fixture` needs no credential; `fixture-a` needs an API key that is only
/// configured when a test asks for it.
fn catalog(workspace_default: Option<ModelRef>) -> ProviderCatalog {
    let models = vec![
        model("fixture", "small", &[Effort::Low, Effort::Off], false),
        model("fixture", "large", &[Effort::Off, Effort::High], false),
        model("fixture", "plain", &[], false),
        model("fixture-a", "large", &[Effort::Low, Effort::Medium], false),
        model("fixture-a", "legacy", &[], true),
    ];
    let providers = vec![
        provider("fixture", AuthKind::None),
        provider("fixture-a", AuthKind::ApiKey),
    ];
    ProviderCatalog::new(7, providers, models, workspace_default).unwrap()
}

fn keyed() -> ConfiguredCredentials {
    ConfiguredCredentials::new([(id("fixture-a"), AuthKind::ApiKey)])
}

fn request(model: &str) -> SelectionRequest {
    SelectionRequest {
        provider_id: None,
        model: ModelRequest::parse(Some(model)).unwrap(),
        effort: None,
        session: None,
    }
}

fn follow_up(model: Option<&str>, session: SessionSelection) -> SelectionRequest {
    SelectionRequest {
        model: ModelRequest::parse(model).unwrap(),
        session: Some(session),
        ..request("auto")
    }
}

#[test]
fn named_model_is_admitted_with_its_default_effort() {
    let mut named = request("small");
    named.provider_id = Some(id("fixture"));
    let admission = catalog(None).admit(&named, &keyed()).unwrap();
    assert_eq!(admission.model, key("fixture", "small"));
    assert_eq!(admission.choice, ModelChoice::Named);
    assert_eq!(
        admission.effort,
        EffortDecision {
            effective: Some(Effort::Low),
            source: EffortSource::ModelDefault
        }
    );
    assert_eq!(admission.attempt, AttemptDecision::NewSession);
}

#[test]
fn unknown_unavailable_and_ambiguous_models_are_refused_distinctly() {
    let catalog = catalog(None);
    let nothing = ConfiguredCredentials::default();
    assert_eq!(
        catalog.admit(&request("huge"), &keyed()),
        Err(AdmissionError::UnknownModel)
    );
    let mut scoped = request("large");
    scoped.provider_id = Some(id("fixture-a"));
    assert_eq!(
        catalog.admit(&scoped, &nothing),
        Err(AdmissionError::ModelUnavailable)
    );
    assert!(catalog.admit(&scoped, &keyed()).is_ok());
    assert_eq!(
        catalog.admit(&request("large"), &keyed()),
        Err(AdmissionError::AmbiguousModel)
    );
    scoped.provider_id = Some(id("fixture-x"));
    assert_eq!(
        catalog.admit(&scoped, &keyed()),
        Err(AdmissionError::UnknownProvider)
    );
    assert_eq!(
        ModelRequest::parse(Some("")),
        Err(CatalogError::InvalidModelId)
    );

    let classes = [
        AdmissionError::UnknownModel,
        AdmissionError::ModelUnavailable,
        AdmissionError::EffortNotOffered,
    ]
    .map(AdmissionError::class);
    assert_eq!(
        classes,
        ["not_found", "model_unavailable", "invalid_request"]
    );
}

#[test]
fn effort_the_model_does_not_offer_is_refused_before_execution() {
    let catalog = catalog(None);
    let mut unoffered = request("small");
    unoffered.effort = Some(Effort::Max);
    assert_eq!(
        catalog.admit(&unoffered, &keyed()),
        Err(AdmissionError::EffortNotOffered)
    );
    let mut effortless = request("plain");
    effortless.effort = Some(Effort::Off);
    assert_eq!(
        catalog.admit(&effortless, &keyed()),
        Err(AdmissionError::EffortNotOffered)
    );
    effortless.effort = None;
    let admission = catalog.admit(&effortless, &keyed()).unwrap();
    assert_eq!(admission.effort.effective, None);
    assert_eq!(admission.effort.source, EffortSource::NotOffered);
    unoffered.effort = Some(Effort::Off);
    let admission = catalog.admit(&unoffered, &keyed()).unwrap();
    assert_eq!(admission.effort.effective, Some(Effort::Off));
    assert_eq!(admission.effort.source, EffortSource::Requested);
}

#[test]
fn auto_and_an_omitted_model_resolve_once_and_are_recorded() {
    let catalog = catalog(Some(key("fixture", "small")));
    let omitted = SelectionRequest {
        model: ModelRequest::Omitted,
        ..request("auto")
    };
    for (selection, requested) in [
        (request("auto"), AutoRequest::Token),
        (omitted, AutoRequest::Omitted),
    ] {
        let admission = catalog.admit(&selection, &keyed()).unwrap();
        assert_eq!(admission.model, key("fixture", "small"));
        assert_eq!(
            admission.choice,
            ModelChoice::Auto(AutoResolution {
                requested,
                resolved: key("fixture", "small"),
                catalog_revision: 7,
                rule: ResolutionRule::WorkspaceDefault,
            })
        );
    }
}

#[test]
fn auto_never_falls_back_to_another_model_or_provider() {
    let nothing = ConfiguredCredentials::default();
    let unresolvable = |cause| Err(AdmissionError::AutoUnresolvable(cause));
    assert_eq!(
        catalog(None).admit(&request("auto"), &nothing),
        unresolvable(AutoCause::NoWorkspaceDefault)
    );
    // Other models are available, but the unavailable default is not replaced.
    let hosted_default = catalog(Some(key("fixture-a", "large")));
    assert_eq!(
        hosted_default.admit(&request("auto"), &nothing),
        unresolvable(AutoCause::DefaultUnavailable)
    );
    let mut scoped = request("auto");
    scoped.provider_id = Some(id("fixture"));
    assert_eq!(
        hosted_default.admit(&scoped, &keyed()),
        unresolvable(AutoCause::DefaultOutsideProvider)
    );
    assert_eq!(
        AdmissionError::AutoUnresolvable(AutoCause::DefaultUnavailable).class(),
        "model_unavailable"
    );
}

#[test]
fn model_override_on_a_follow_up_starts_a_new_attempt() {
    let catalog = catalog(None);
    let session = SessionSelection {
        model: key("fixture", "small"),
        effort: Some(Effort::Off),
    };
    let admission = catalog
        .admit(&follow_up(Some("large"), session.clone()), &keyed())
        .unwrap_err();
    assert_eq!(admission, AdmissionError::AmbiguousModel);
    let mut overridden = follow_up(Some("large"), session.clone());
    overridden.provider_id = Some(id("fixture"));
    let admission = catalog.admit(&overridden, &keyed()).unwrap();
    assert_eq!(admission.model, key("fixture", "large"));
    assert_eq!(admission.effort.source, EffortSource::KeptFromSession);
    assert_eq!(
        admission.attempt,
        AttemptDecision::NewAttempt {
            previous: session.clone()
        }
    );
    let mut new_effort = follow_up(None, session.clone());
    new_effort.effort = Some(Effort::Low);
    let admission = catalog.admit(&new_effort, &keyed()).unwrap();
    assert_eq!(admission.choice, ModelChoice::KeptFromSession);
    assert!(matches!(
        admission.attempt,
        AttemptDecision::NewAttempt { .. }
    ));
    let unchanged = catalog.admit(&follow_up(None, session), &keyed()).unwrap();
    assert_eq!(unchanged.attempt, AttemptDecision::SameSelection);
    assert_eq!(unchanged.effort.effective, Some(Effort::Off));
}

#[test]
fn kept_session_effort_must_be_offered_by_the_overriding_model() {
    let session = SessionSelection {
        model: key("fixture", "large"),
        effort: Some(Effort::High),
    };
    let mut overridden = follow_up(Some("small"), session);
    let catalog = catalog(None);
    assert_eq!(
        catalog.admit(&overridden, &keyed()),
        Err(AdmissionError::EffortNotOffered)
    );
    overridden.effort = Some(Effort::Low);
    assert!(catalog.admit(&overridden, &keyed()).is_ok());
}

#[test]
fn hidden_model_only_continues_a_session_that_uses_it() {
    let catalog = catalog(None);
    let mut fresh = request("legacy");
    fresh.provider_id = Some(id("fixture-a"));
    assert_eq!(
        catalog.admit(&fresh, &keyed()),
        Err(AdmissionError::HiddenModel)
    );
    let session = SessionSelection {
        model: key("fixture-a", "legacy"),
        effort: None,
    };
    let continued = catalog
        .admit(&follow_up(None, session.clone()), &keyed())
        .unwrap();
    assert_eq!(continued.model, key("fixture-a", "legacy"));
    assert_eq!(continued.attempt, AttemptDecision::SameSelection);
    let other = SessionSelection {
        model: key("fixture", "small"),
        effort: Some(Effort::Low),
    };
    assert_eq!(
        catalog.admit(&follow_up(Some("legacy"), other), &keyed()),
        Err(AdmissionError::HiddenModel)
    );
    // Continuing still needs the provider to be available now.
    assert_eq!(
        catalog.admit(&follow_up(None, session), &ConfiguredCredentials::default()),
        Err(AdmissionError::ModelUnavailable)
    );
}

#[test]
fn a_session_model_missing_from_the_catalog_is_not_kept_silently() {
    let session = SessionSelection {
        model: key("fixture", "retired"),
        effort: None,
    };
    assert_eq!(
        catalog(None).admit(&follow_up(None, session), &keyed()),
        Err(AdmissionError::UnknownModel)
    );
}
