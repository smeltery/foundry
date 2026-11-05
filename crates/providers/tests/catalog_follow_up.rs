mod catalog_support;

use catalog_support::{catalog, follow_up, id, key, keyed, model, provider, request};
use foundry_providers::catalog::{
    AdmissionError, AttemptDecision, AuthKind, AutoRequest, AutoResolution, ConfiguredCredentials,
    Effort, EffortSource, ModelChoice, ProviderCatalog, ResolutionRule, SessionSelection,
};

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
    let named = catalog
        .admit(&follow_up(Some("legacy"), session.clone()), &keyed())
        .unwrap();
    assert_eq!(named.model, key("fixture-a", "legacy"));
    let other = SessionSelection {
        model: key("fixture", "small"),
        effort: Some(Effort::Low),
    };
    // A bare ID does not see another session's hidden model at all.
    assert_eq!(
        catalog.admit(&follow_up(Some("legacy"), other.clone()), &keyed()),
        Err(AdmissionError::UnknownModel)
    );
    let mut scoped = follow_up(Some("legacy"), other);
    scoped.provider_id = Some(id("fixture-a"));
    assert_eq!(
        catalog.admit(&scoped, &keyed()),
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

#[test]
fn hidden_model_does_not_make_a_bare_id_ambiguous() {
    let providers = vec![
        provider("fixture", AuthKind::None),
        provider("fixture-a", AuthKind::None),
    ];
    let models = vec![
        model("fixture", "shared", &[], false),
        model("fixture-a", "shared", &[], true),
    ];
    let catalog = ProviderCatalog::new(1, providers, models, None).unwrap();
    let nothing = ConfiguredCredentials::default();
    let admission = catalog.admit(&request("shared"), &nothing).unwrap();
    assert_eq!(admission.model, key("fixture", "shared"));
    // A session on the hidden model sees both, so the bare ID is ambiguous there.
    let session = SessionSelection {
        model: key("fixture-a", "shared"),
        effort: None,
    };
    assert_eq!(
        catalog.admit(&follow_up(Some("shared"), session), &nothing),
        Err(AdmissionError::AmbiguousModel)
    );
}

#[test]
fn session_effort_is_never_dropped_for_a_model_without_efforts() {
    let catalog = catalog(None);
    let session = SessionSelection {
        model: key("fixture", "small"),
        effort: Some(Effort::Low),
    };
    assert_eq!(
        catalog.admit(&follow_up(Some("plain"), session), &keyed()),
        Err(AdmissionError::EffortNotOffered)
    );
    let effortless = SessionSelection {
        model: key("fixture", "plain"),
        effort: None,
    };
    let admission = catalog
        .admit(&follow_up(None, effortless), &keyed())
        .unwrap();
    assert_eq!(admission.effort.source, EffortSource::NotOffered);
    assert_eq!(admission.attempt, AttemptDecision::SameSelection);
}

#[test]
fn follow_up_naming_another_provider_without_a_model_is_a_mismatch() {
    let session = SessionSelection {
        model: key("fixture", "small"),
        effort: Some(Effort::Low),
    };
    let mut mismatched = follow_up(None, session);
    mismatched.provider_id = Some(id("fixture-a"));
    let refused = catalog(None).admit(&mismatched, &keyed()).unwrap_err();
    assert_eq!(refused, AdmissionError::ProviderMismatch);
    assert_eq!(refused.class(), "invalid_request");
}

#[test]
fn explicit_auto_on_a_follow_up_is_recorded_and_changes_attempts_only_when_needed() {
    let catalog = catalog(Some(key("fixture", "small")));
    let resolution = ModelChoice::Auto(AutoResolution {
        requested: AutoRequest::Token,
        resolved: key("fixture", "small"),
        catalog_revision: 7,
        rule: ResolutionRule::WorkspaceDefault,
    });
    let on_default = SessionSelection {
        model: key("fixture", "small"),
        effort: Some(Effort::Low),
    };
    let same = catalog
        .admit(&follow_up(Some("auto"), on_default), &keyed())
        .unwrap();
    assert_eq!(same.choice, resolution);
    assert_eq!(same.attempt, AttemptDecision::SameSelection);

    let elsewhere = SessionSelection {
        model: key("fixture", "large"),
        effort: Some(Effort::Off),
    };
    let moved = catalog
        .admit(&follow_up(Some("auto"), elsewhere.clone()), &keyed())
        .unwrap();
    assert_eq!(moved.model, key("fixture", "small"));
    assert_eq!(moved.choice, resolution);
    assert_eq!(moved.effort.effective, Some(Effort::Off));
    assert_eq!(
        moved.attempt,
        AttemptDecision::NewAttempt {
            previous: elsewhere
        }
    );
}
