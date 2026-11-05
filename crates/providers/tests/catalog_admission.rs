mod catalog_support;

use catalog_support::{catalog, id, key, keyed, request};
use foundry_providers::catalog::{
    AdmissionError, AttemptDecision, AutoCause, AutoRequest, AutoResolution, CatalogError,
    ConfiguredCredentials, Effort, EffortDecision, EffortSource, ModelChoice, ModelRequest,
    ResolutionRule, SelectionRequest,
};

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
fn every_admission_error_maps_to_its_specified_class() {
    let cases = [
        (
            AdmissionError::InvalidRequest(CatalogError::InvalidModelId),
            "invalid_request",
        ),
        (AdmissionError::AmbiguousModel, "invalid_request"),
        (AdmissionError::ProviderMismatch, "invalid_request"),
        (AdmissionError::EffortNotOffered, "invalid_request"),
        (AdmissionError::UnknownProvider, "not_found"),
        (AdmissionError::UnknownModel, "not_found"),
        (AdmissionError::HiddenModel, "not_found"),
        (AdmissionError::ModelUnavailable, "model_unavailable"),
        (
            AdmissionError::AutoUnresolvable(AutoCause::NoWorkspaceDefault),
            "model_unavailable",
        ),
        (
            AdmissionError::AutoUnresolvable(AutoCause::DefaultUnavailable),
            "model_unavailable",
        ),
        (
            AdmissionError::AutoUnresolvable(AutoCause::DefaultOutsideProvider),
            "model_unavailable",
        ),
    ];
    for (index, (error, class)) in cases.iter().enumerate() {
        assert_eq!(error.class(), *class, "{error:?}");
        // Variants that share a class stay distinguishable to callers.
        for (other, _) in &cases[index + 1..] {
            assert_ne!(error, other);
        }
    }
}

#[test]
fn unavailability_is_reported_before_an_effort_mismatch() {
    let mut both = request("large");
    both.provider_id = Some(id("fixture-a"));
    both.effort = Some(Effort::Max);
    let catalog = catalog(None);
    assert_eq!(
        catalog.admit(&both, &ConfiguredCredentials::default()),
        Err(AdmissionError::ModelUnavailable)
    );
    assert_eq!(
        catalog.admit(&both, &keyed()),
        Err(AdmissionError::EffortNotOffered)
    );
}
