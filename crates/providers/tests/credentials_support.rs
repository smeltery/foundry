#![allow(dead_code)]
//! Fixture catalog and helpers shared by the credential test files, included
//! with `#[path]`. Neutral fixture providers and synthetic canary keys only.

use foundry_identity::authz::{PrincipalId, Role, WorkspaceId};
use foundry_providers::catalog::{
    AuthKind, ConfigField, LoginUnavailable, OauthLogin, ProviderCatalog, ProviderDescriptor,
    ProviderId, ProviderKind,
};
use foundry_providers::credentials::{
    AccountKey, Caller, ConfigureRequest, CredentialStore, Secret,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A synthetic key that must never appear in any output.
pub const CANARY: &str = "canary-5d1e-write-only-material";
pub const OTHER_CANARY: &str = "canary-a83f-replacement-material";
/// 2026-01-01T00:00:00Z.
pub const NOW_MS: u64 = 1_767_225_600_000;

pub fn id(value: &str) -> ProviderId {
    ProviderId::new(value).unwrap()
}

pub fn account(value: &str) -> AccountKey {
    AccountKey::new(value).unwrap()
}

pub fn secret(value: &str) -> Secret {
    Secret::new(value.to_owned()).unwrap()
}

fn field(name: &str, required: bool) -> ConfigField {
    ConfigField {
        name: name.into(),
        label: name.into(),
        required,
    }
}

fn provider(
    name: &str,
    kind: ProviderKind,
    auth: AuthKind,
    fields: Vec<ConfigField>,
) -> ProviderDescriptor {
    ProviderDescriptor {
        provider_id: id(name),
        kind,
        auth_kinds: vec![auth],
        config_fields: fields,
        oauth_login: OauthLogin::Unavailable(LoginUnavailable::NotSupported),
        efforts: vec![],
        endpoint_format: None,
    }
}

/// `fixture` takes a bare key, `hosted-a` an optional `region`, `hosted-b` a
/// required `tier`, and `local-a` no credential at all.
pub fn catalog() -> ProviderCatalog {
    let providers = vec![
        provider("fixture", ProviderKind::Fixture, AuthKind::ApiKey, vec![]),
        provider(
            "hosted-a",
            ProviderKind::HostedApi,
            AuthKind::ApiKey,
            vec![field("region", false)],
        ),
        provider(
            "hosted-b",
            ProviderKind::HostedApi,
            AuthKind::ApiKey,
            vec![field("tier", true)],
        ),
        provider(
            "local-a",
            ProviderKind::LocalEndpoint,
            AuthKind::None,
            vec![],
        ),
    ];
    ProviderCatalog::new(1, providers, vec![], None).unwrap()
}

pub fn caller(workspace: &str, principal: &str, role: Role) -> Caller {
    Caller {
        workspace: WorkspaceId::new(workspace),
        principal: PrincipalId::new(principal),
        role,
    }
}

pub fn owner() -> Caller {
    caller("workspace-a", "owner-a", Role::Owner)
}

pub fn path(directory: &Path) -> PathBuf {
    directory.join("credentials.sqlite")
}

pub fn open(directory: &Path) -> CredentialStore {
    CredentialStore::open_with_clock(&path(directory), Arc::new(|| NOW_MS)).unwrap()
}

pub fn request(
    provider_id: &str,
    account_key: Option<&str>,
    key: &str,
    config: &[(&str, &str)],
) -> ConfigureRequest {
    ConfigureRequest {
        provider_id: id(provider_id),
        account_key: account_key.map(account),
        secret: secret(key),
        config: config
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
    }
}

/// The handed-off value of one slot, for comparing which key a slot holds.
pub fn stored_value(
    store: &CredentialStore,
    caller: &Caller,
    provider_id: &str,
    key: &str,
) -> String {
    let handoff = store
        .handoff(caller, &id(provider_id), &account(key))
        .unwrap();
    handoff.into_adapter_parts().1
}

pub fn keys(store: &CredentialStore, who: &Caller) -> Vec<(String, String)> {
    store
        .list(who)
        .unwrap()
        .items
        .into_iter()
        .map(|slot| {
            (
                slot.provider_id.to_string(),
                slot.account_key.as_str().to_owned(),
            )
        })
        .collect()
}

pub fn pairs(values: &[(&str, &str)]) -> Vec<(String, String)> {
    values
        .iter()
        .map(|(p, a)| ((*p).to_owned(), (*a).to_owned()))
        .collect()
}
