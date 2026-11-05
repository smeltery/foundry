#![allow(dead_code)]
//! Fixture catalog shared by the admission test files. Fixture providers only.

use foundry_providers::catalog::{
    AuthKind, ConfiguredCredentials, Effort, LoginUnavailable, ModelDescriptor, ModelId, ModelRef,
    ModelRequest, OauthLogin, ProviderCatalog, ProviderDescriptor, ProviderId, ProviderKind,
    SelectionRequest, SessionSelection,
};

pub fn id(value: &str) -> ProviderId {
    ProviderId::new(value).unwrap()
}

pub fn key(provider_id: &str, model_id: &str) -> ModelRef {
    ModelRef {
        provider_id: id(provider_id),
        model_id: ModelId::new(model_id).unwrap(),
    }
}

pub fn provider(provider_id: &str, auth_kind: AuthKind) -> ProviderDescriptor {
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

pub fn model(
    provider_id: &str,
    model_id: &str,
    efforts: &[Effort],
    hidden: bool,
) -> ModelDescriptor {
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
pub fn catalog(workspace_default: Option<ModelRef>) -> ProviderCatalog {
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

pub fn keyed() -> ConfiguredCredentials {
    ConfiguredCredentials::new([(id("fixture-a"), AuthKind::ApiKey)])
}

pub fn request(model: &str) -> SelectionRequest {
    SelectionRequest {
        provider_id: None,
        model: ModelRequest::parse(Some(model)).unwrap(),
        effort: None,
        session: None,
    }
}

pub fn follow_up(model: Option<&str>, session: SessionSelection) -> SelectionRequest {
    SelectionRequest {
        model: ModelRequest::parse(model).unwrap(),
        session: Some(session),
        ..request("auto")
    }
}
