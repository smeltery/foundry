use foundry_providers::catalog::{
    AccountMode, AuthKind, CatalogError, ConfigField, ConfiguredCredentials, Effort,
    EndpointFormat, LoginMethod, LoginUnavailable, ModelDescriptor, ModelId, ModelRef, OauthLogin,
    ProviderCatalog, ProviderDescriptor, ProviderId, ProviderKind,
};

fn id(value: &str) -> ProviderId {
    ProviderId::new(value).unwrap()
}

fn offline(provider_id: &str) -> ProviderDescriptor {
    ProviderDescriptor {
        provider_id: id(provider_id),
        kind: ProviderKind::Fixture,
        auth_kinds: vec![AuthKind::None],
        config_fields: vec![],
        oauth_login: OauthLogin::Unavailable(LoginUnavailable::NotSupported),
        efforts: vec![Effort::Off, Effort::Low, Effort::High],
        endpoint_format: None,
    }
}

fn keyed(provider_id: &str) -> ProviderDescriptor {
    ProviderDescriptor {
        provider_id: id(provider_id),
        kind: ProviderKind::HostedApi,
        auth_kinds: vec![AuthKind::ApiKey, AuthKind::Oauth],
        config_fields: vec![ConfigField {
            name: "region".into(),
            label: "Region".into(),
            required: false,
        }],
        oauth_login: OauthLogin::Available {
            methods: vec![LoginMethod::Browser, LoginMethod::DeviceCode],
            default_method: LoginMethod::DeviceCode,
            account_mode: AccountMode::Multiple,
        },
        efforts: vec![Effort::Minimal, Effort::Low, Effort::Medium, Effort::High],
        endpoint_format: Some(EndpointFormat::Response),
    }
}

fn model(provider_id: &str, model_id: &str, efforts: &[Effort]) -> ModelDescriptor {
    ModelDescriptor {
        provider_id: id(provider_id),
        model_id: ModelId::new(model_id).unwrap(),
        label: format!("Model {model_id}"),
        efforts: efforts.to_vec(),
        default_effort: efforts.first().copied(),
        hidden: false,
    }
}

fn build(
    providers: Vec<ProviderDescriptor>,
    models: Vec<ModelDescriptor>,
) -> Result<ProviderCatalog, CatalogError> {
    ProviderCatalog::new(1, providers, models, None)
}

#[test]
fn providers_are_listed_in_id_order_without_credential_state() {
    let catalog = build(vec![keyed("fixture-a"), offline("fixture")], vec![]).unwrap();
    let ids: Vec<_> = catalog
        .providers()
        .map(|p| p.provider_id.as_str())
        .collect();
    assert_eq!(ids, ["fixture", "fixture-a"]);
    let listed = catalog.provider(&id("fixture-a")).unwrap();
    assert_eq!(listed, &keyed("fixture-a"));
}

#[test]
fn provider_ids_are_bounded_neutral_labels() {
    for valid in ["a", "fixture-a", "f0-x", &"a".repeat(64)] {
        assert!(ProviderId::new(valid).is_ok(), "{valid}");
    }
    for invalid in [
        "",
        "0fixture",
        "-fixture",
        "Fixture",
        "fixture_a",
        "fix ture",
        &"a".repeat(65),
    ] {
        assert_eq!(
            ProviderId::new(invalid),
            Err(CatalogError::InvalidProviderId),
            "{invalid}"
        );
    }
}

#[test]
fn model_ids_are_bounded_and_never_the_auto_token() {
    assert!(ModelId::new(&"m".repeat(256)).is_ok());
    for invalid in [
        "",
        "  ",
        "auto",
        "with\0nul",
        "line\nbreak",
        &"m".repeat(257),
    ] {
        assert_eq!(ModelId::new(invalid), Err(CatalogError::InvalidModelId));
    }
}

#[test]
fn effort_vocabulary_is_closed() {
    for effort in Effort::ALL {
        assert_eq!(Effort::parse(effort.as_str()), Ok(effort));
    }
    for word in ["automatic", "auto", "HIGH", ""] {
        assert_eq!(Effort::parse(word), Err(CatalogError::UnknownEffort));
    }
}

#[test]
fn auth_kinds_must_be_nonempty_unique_and_none_only_offline() {
    let mut empty = keyed("fixture-a");
    empty.auth_kinds.clear();
    let mut repeated = keyed("fixture-a");
    repeated.auth_kinds.push(AuthKind::ApiKey);
    let mut hosted_none = keyed("fixture-a");
    hosted_none.auth_kinds.push(AuthKind::None);
    for descriptor in [empty, repeated, hosted_none] {
        assert_eq!(descriptor.validate(), Err(CatalogError::InvalidAuthKinds));
    }
    let mut local = offline("fixture-local");
    local.kind = ProviderKind::LocalEndpoint;
    local.endpoint_format = Some(EndpointFormat::default());
    assert_eq!(local.validate(), Ok(()));
    assert_eq!(EndpointFormat::default(), EndpointFormat::Chat);
}

#[test]
fn config_fields_are_non_secret_api_key_settings() {
    let mut duplicate = keyed("fixture-a");
    duplicate
        .config_fields
        .push(duplicate.config_fields[0].clone());
    let mut badly_named = keyed("fixture-a");
    badly_named.config_fields[0].name = "Region".into();
    let mut unlabeled = keyed("fixture-a");
    unlabeled.config_fields[0].label = " ".into();
    let mut without_api_key = keyed("fixture-a");
    without_api_key.auth_kinds = vec![AuthKind::Oauth];
    for descriptor in [duplicate, badly_named, unlabeled, without_api_key] {
        assert_eq!(
            descriptor.validate(),
            Err(CatalogError::InvalidConfigFields)
        );
    }
}

#[test]
fn oauth_login_needs_oauth_and_a_listed_default_method() {
    let mut without_oauth = keyed("fixture-a");
    without_oauth.auth_kinds = vec![AuthKind::ApiKey];
    let mut disabled_without_oauth = without_oauth.clone();
    disabled_without_oauth.oauth_login = OauthLogin::Unavailable(LoginUnavailable::NotEnabled);
    let mut unlisted_default = keyed("fixture-a");
    unlisted_default.oauth_login = OauthLogin::Available {
        methods: vec![LoginMethod::Browser],
        default_method: LoginMethod::Interactive,
        account_mode: AccountMode::Single,
    };
    let mut no_methods = keyed("fixture-a");
    no_methods.oauth_login = OauthLogin::Available {
        methods: vec![],
        default_method: LoginMethod::Browser,
        account_mode: AccountMode::Single,
    };
    for descriptor in [
        without_oauth,
        disabled_without_oauth,
        unlisted_default,
        no_methods,
    ] {
        assert_eq!(descriptor.validate(), Err(CatalogError::InvalidOauthLogin));
    }
    // Supporting OAuth does not by itself make a login available.
    let mut not_enabled = keyed("fixture-b");
    not_enabled.oauth_login = OauthLogin::Unavailable(LoginUnavailable::NotEnabled);
    assert_eq!(not_enabled.validate(), Ok(()));
}

#[test]
fn endpoint_format_is_explicit_for_local_endpoints_and_absent_for_fixtures() {
    let mut local = offline("fixture-local");
    local.kind = ProviderKind::LocalEndpoint;
    let mut fixture_with_format = offline("fixture");
    fixture_with_format.endpoint_format = Some(EndpointFormat::Message);
    for descriptor in [local, fixture_with_format] {
        assert_eq!(
            descriptor.validate(),
            Err(CatalogError::InvalidEndpointFormat)
        );
    }
}

#[test]
fn registration_rejects_duplicates_and_orphans() {
    let duplicate = build(vec![offline("fixture"), offline("fixture")], vec![]);
    assert_eq!(duplicate, Err(CatalogError::DuplicateProvider));
    let small = model("fixture", "small", &[Effort::Off]);
    let twice = build(vec![offline("fixture")], vec![small.clone(), small]);
    assert_eq!(twice, Err(CatalogError::DuplicateModel));
    let orphan = build(
        vec![offline("fixture")],
        vec![model("fixture-x", "small", &[])],
    );
    assert_eq!(orphan, Err(CatalogError::ModelProviderUnknown));
}

#[test]
fn model_efforts_stay_within_the_provider_and_name_a_default() {
    let outside = model("fixture", "small", &[Effort::Off, Effort::Max]);
    let mut missing_default = model("fixture", "small", &[Effort::Low]);
    missing_default.default_effort = None;
    let mut unoffered_default = model("fixture", "small", &[Effort::Low]);
    unoffered_default.default_effort = Some(Effort::High);
    let mut default_without_efforts = model("fixture", "small", &[]);
    default_without_efforts.default_effort = Some(Effort::Off);
    let mut repeated = model("fixture", "small", &[Effort::Low]);
    repeated.efforts.push(Effort::Low);
    let mut unlabeled = model("fixture", "small", &[]);
    unlabeled.label = String::new();
    for (descriptor, error) in [
        (outside, CatalogError::ModelEffortOutsideProvider),
        (missing_default, CatalogError::InvalidDefaultEffort),
        (unoffered_default, CatalogError::InvalidDefaultEffort),
        (default_without_efforts, CatalogError::InvalidDefaultEffort),
        (repeated, CatalogError::InvalidEfforts),
        (unlabeled, CatalogError::InvalidLabel),
    ] {
        assert_eq!(
            build(vec![offline("fixture")], vec![descriptor]),
            Err(error)
        );
    }
}

#[test]
fn workspace_default_must_be_a_listed_model() {
    let mut hidden = model("fixture", "legacy", &[]);
    hidden.hidden = true;
    let key = |model_id: &str| ModelRef {
        provider_id: id("fixture"),
        model_id: ModelId::new(model_id).unwrap(),
    };
    let unknown = ProviderCatalog::new(1, vec![offline("fixture")], vec![], Some(key("small")));
    assert_eq!(unknown, Err(CatalogError::UnknownWorkspaceDefault));
    let hidden = ProviderCatalog::new(
        1,
        vec![offline("fixture")],
        vec![hidden],
        Some(key("legacy")),
    );
    assert_eq!(hidden, Err(CatalogError::HiddenWorkspaceDefault));
}

#[test]
fn availability_follows_configured_credentials_of_an_accepted_kind() {
    let mut key_only = keyed("fixture-b");
    key_only.auth_kinds = vec![AuthKind::ApiKey];
    key_only.oauth_login = OauthLogin::Unavailable(LoginUnavailable::NotSupported);
    let catalog = build(
        vec![offline("fixture"), keyed("fixture-a"), key_only],
        vec![],
    )
    .unwrap();
    let nothing = ConfiguredCredentials::default();
    assert_eq!(
        catalog.provider_available(&id("fixture"), &nothing),
        Ok(true)
    );
    assert_eq!(
        catalog.provider_available(&id("fixture-a"), &nothing),
        Ok(false)
    );
    let oauth = ConfiguredCredentials::new([
        (id("fixture-a"), AuthKind::Oauth),
        (id("fixture-b"), AuthKind::Oauth),
    ]);
    assert_eq!(
        catalog.provider_available(&id("fixture-a"), &oauth),
        Ok(true)
    );
    // An OAuth credential does not serve a provider that only accepts API keys.
    assert_eq!(
        catalog.provider_available(&id("fixture-b"), &oauth),
        Ok(false)
    );
    assert!(
        catalog
            .provider_available(&id("fixture-x"), &oauth)
            .is_err()
    );
}

#[test]
fn model_list_is_ordered_flags_availability_and_omits_hidden_models() {
    let mut legacy = model("fixture-a", "legacy", &[]);
    legacy.hidden = true;
    let models = vec![
        model(
            "fixture-a",
            "large",
            &[Effort::Low, Effort::Medium, Effort::High],
        ),
        legacy,
        model("fixture", "small", &[Effort::Off, Effort::Low]),
        model("fixture", "large", &[]),
    ];
    let catalog = build(vec![keyed("fixture-a"), offline("fixture")], models).unwrap();
    let listed: Vec<_> = catalog
        .models(&ConfiguredCredentials::default())
        .into_iter()
        .map(|entry| {
            let key = entry.model.key();
            (
                format!("{}/{}", key.provider_id, key.model_id),
                entry.available,
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("fixture/large".to_owned(), true),
            ("fixture/small".to_owned(), true),
            ("fixture-a/large".to_owned(), false),
        ]
    );
}
