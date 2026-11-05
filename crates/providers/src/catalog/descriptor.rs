use super::{CatalogError, Effort};
use std::collections::BTreeSet;
use std::fmt;

/// A neutral Foundry provider label: 1 to 64 lowercase ASCII letters, digits and
/// hyphens, starting with a letter. It never names a real service.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn new(value: &str) -> Result<Self, CatalogError> {
        if is_identifier(value, 64, b'-') {
            Ok(Self(value.to_owned()))
        } else {
            Err(CatalogError::InvalidProviderId)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderKind {
    HostedApi,
    LocalEndpoint,
    Fixture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AuthKind {
    ApiKey,
    Oauth,
    None,
}

/// Request shape a provider endpoint speaks. Names are provisional wire names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EndpointFormat {
    Chat,
    Response,
    Message,
}

/// A non-secret setting an API-key connection takes. Secret values are never
/// config fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigField {
    pub name: String,
    pub label: String,
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LoginMethod {
    Browser,
    DeviceCode,
    Interactive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountMode {
    Single,
    Multiple,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginUnavailable {
    NotSupported,
    NotEnabled,
}

/// Whether a login can start. Supporting OAuth as an auth kind alone never makes
/// a login available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OauthLogin {
    Available {
        methods: Vec<LoginMethod>,
        default_method: LoginMethod,
        account_mode: AccountMode,
    },
    Unavailable(LoginUnavailable),
}

/// Public, secret-free provider configuration. It carries no keys, tokens,
/// account labels or usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub provider_id: ProviderId,
    pub kind: ProviderKind,
    pub auth_kinds: Vec<AuthKind>,
    pub config_fields: Vec<ConfigField>,
    pub oauth_login: OauthLogin,
    /// The effort vocabulary this provider's models may offer.
    pub efforts: Vec<Effort>,
    /// A local endpoint without one is registered with `Chat`, the specified
    /// default. A fixture takes none.
    pub endpoint_format: Option<EndpointFormat>,
}

impl ProviderDescriptor {
    /// The format a local endpoint uses when its descriptor names none.
    pub const LOCAL_ENDPOINT_FORMAT: EndpointFormat = EndpointFormat::Chat;

    /// Fills in specified defaults, then validates.
    pub(super) fn normalize(mut self) -> Result<Self, CatalogError> {
        if self.kind == ProviderKind::LocalEndpoint && self.endpoint_format.is_none() {
            self.endpoint_format = Some(Self::LOCAL_ENDPOINT_FORMAT);
        }
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.auth_kinds.is_empty()
            || !is_unique(&self.auth_kinds)
            || (self.accepts(AuthKind::None) && self.kind == ProviderKind::HostedApi)
        {
            return Err(CatalogError::InvalidAuthKinds);
        }
        self.validate_config_fields()?;
        self.validate_oauth_login()?;
        if !is_unique(&self.efforts) {
            return Err(CatalogError::InvalidEfforts);
        }
        if self.kind == ProviderKind::Fixture && self.endpoint_format.is_some() {
            return Err(CatalogError::InvalidEndpointFormat);
        }
        Ok(())
    }

    pub fn accepts(&self, kind: AuthKind) -> bool {
        self.auth_kinds.contains(&kind)
    }

    pub fn offers(&self, effort: Effort) -> bool {
        self.efforts.contains(&effort)
    }

    fn validate_config_fields(&self) -> Result<(), CatalogError> {
        let mut names = BTreeSet::new();
        for field in &self.config_fields {
            if !is_identifier(&field.name, 64, b'_')
                || !is_label(&field.label)
                || !names.insert(field.name.as_str())
            {
                return Err(CatalogError::InvalidConfigFields);
            }
        }
        if !self.config_fields.is_empty() && !self.accepts(AuthKind::ApiKey) {
            return Err(CatalogError::InvalidConfigFields);
        }
        Ok(())
    }

    fn validate_oauth_login(&self) -> Result<(), CatalogError> {
        match &self.oauth_login {
            OauthLogin::Available {
                methods,
                default_method,
                ..
            } => {
                if !self.accepts(AuthKind::Oauth)
                    || methods.is_empty()
                    || !is_unique(methods)
                    || !methods.contains(default_method)
                {
                    return Err(CatalogError::InvalidOauthLogin);
                }
            }
            // Only a provider that accepts OAuth can have its login switched off.
            OauthLogin::Unavailable(LoginUnavailable::NotEnabled)
                if !self.accepts(AuthKind::Oauth) =>
            {
                return Err(CatalogError::InvalidOauthLogin);
            }
            OauthLogin::Unavailable(_) => {}
        }
        Ok(())
    }
}

/// Lowercase ASCII letters, digits and one separator, starting with a letter.
pub(super) fn is_identifier(value: &str, max_len: usize, separator: u8) -> bool {
    let bytes = value.as_bytes();
    (1..=max_len).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == separator)
}

pub(super) fn is_label(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

pub(super) fn is_unique<T: Ord>(values: &[T]) -> bool {
    values.iter().collect::<BTreeSet<_>>().len() == values.len()
}
