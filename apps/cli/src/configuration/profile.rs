use super::{ConfigFailure, Identity, Origin, PendingCause, Selection, profile_types::*};
use serde_json::Value;

pub(super) fn changes(args: &Value) -> Result<ProfileChanges, ConfigFailure> {
    Ok(ProfileChanges {
        name: scalar(&args["name"])?,
        folder: scalar(&args["folder"])?,
        proxy_ca_file: scalar(&args["proxy_ca_file"])?,
        user_chrome: toggle(&args["user_chrome"])?,
        local_logins: toggle(&args["local_logins"])?,
        keep_awake: toggle(&args["keep_awake"])?,
        minimal: match &args["minimal"] {
            Value::Null => false,
            Value::Bool(value) => *value,
            _ => return Err(ConfigFailure::InvalidRequest),
        },
        add_folder: folders(&args["add_folder"])?,
        remove_folder: folders(&args["remove_folder"])?,
    })
}
fn scalar(value: &Value) -> Result<Option<String>, ConfigFailure> {
    match value {
        Value::Null => Ok(None),
        Value::String(value) => Ok(Some(value.clone())),
        _ => Err(ConfigFailure::InvalidRequest),
    }
}
fn toggle(value: &Value) -> Result<Option<bool>, ConfigFailure> {
    match value.as_str() {
        Some("on") => Ok(Some(true)),
        Some("off") => Ok(Some(false)),
        None if value.is_null() => Ok(None),
        _ => Err(ConfigFailure::InvalidRequest),
    }
}
fn folders(value: &Value) -> Result<Vec<String>, ConfigFailure> {
    match value {
        Value::Null => Ok(Vec::new()),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or(ConfigFailure::InvalidRequest)
            })
            .collect(),
        _ => Err(ConfigFailure::InvalidRequest),
    }
}

pub(super) fn resolve(
    facts: Option<ProfileFacts>,
    changes: ProfileChanges,
    identity: &Selection<Identity>,
) -> ConnectProfile {
    let key = match identity {
        Selection::Selected(identity) => Some(matches!(
            identity.credential.origin,
            Origin::ExplicitKey | Origin::EnvironmentKey
        )),
        Selection::Pending {
            cause: PendingCause::Missing,
            candidates,
        } if candidates.is_empty()
            && facts
                .as_ref()
                .is_some_and(|facts| facts.authentication == ProfileAuthentication::WithoutKey) =>
        {
            Some(false)
        }
        _ => None,
    };
    let platform = facts.as_ref().map(|facts| facts.platform);
    let fresh = facts
        .as_ref()
        .is_some_and(|facts| matches!(facts.lifecycle, ProfileLifecycle::Fresh));
    let mut default_name = None;
    let mut default_folder = None;
    let mut saved = SavedProfile::default();
    if let Some(facts) = facts {
        match facts.lifecycle {
            ProfileLifecycle::Fresh => {
                default_name = facts.hostname.map(|value| (value, ProfileOrigin::Hostname));
                default_folder = match facts.kind {
                    ProfileKind::Personal => facts.home.map(|value| (value, ProfileOrigin::Home)),
                    ProfileKind::Machine => facts.cwd.map(|value| (value, ProfileOrigin::Cwd)),
                    ProfileKind::Unknown => None,
                };
            }
            ProfileLifecycle::Retained(values) => saved = values,
        }
    }
    let mac_default = (fresh && platform == Some(ProfilePlatform::MacOs))
        .then_some((true, ProfileOrigin::PlatformDefault));
    let local_default = if fresh {
        key.map(|key| (!key, ProfileOrigin::AuthenticationDefault))
    } else {
        None
    };
    let mut local_logins = pick(changes.local_logins, saved.local_logins, local_default);
    if key != Some(false)
        && matches!(&local_logins,
        ProfileValue::Selected(candidate) if candidate.value)
    {
        local_logins = pending(local_logins);
    }
    ConnectProfile {
        name: scalar_selection(changes.name.clone(), saved.name, default_name),
        folder: scalar_selection(changes.folder.clone(), saved.folder, default_folder),
        proxy_ca_file: scalar_selection(changes.proxy_ca_file.clone(), saved.proxy_ca_file, None),
        user_chrome: platform_value(
            pick(changes.user_chrome, saved.user_chrome, mac_default),
            platform,
        ),
        keep_awake: platform_value(
            pick(changes.keep_awake, saved.keep_awake, mac_default),
            platform,
        ),
        local_logins,
        minimal: pick(changes.minimal.then_some(true), saved.minimal, None),
        changes,
    }
}
fn pick<T>(
    explicit: Option<T>,
    saved: Option<T>,
    default: Option<(T, ProfileOrigin)>,
) -> ProfileValue<T> {
    let selected = explicit
        .map(|value| (value, ProfileOrigin::Explicit))
        .or_else(|| saved.map(|value| (value, ProfileOrigin::Retained)))
        .or(default);
    match selected {
        Some((value, origin)) => ProfileValue::Selected(ProfileCandidate { value, origin }),
        None => ProfileValue::Pending {
            cause: PendingCause::Missing,
            candidates: Vec::new(),
        },
    }
}
fn pending<T>(value: ProfileValue<T>) -> ProfileValue<T> {
    match value {
        ProfileValue::Selected(candidate) => ProfileValue::Pending {
            cause: PendingCause::OwnerPolicy,
            candidates: vec![candidate],
        },
        pending => pending,
    }
}
fn scalar_selection(
    explicit: Option<String>,
    saved: Option<String>,
    default: Option<(String, ProfileOrigin)>,
) -> ProfileValue<String> {
    if explicit.as_ref().is_some_and(String::is_empty) {
        let mut candidates = vec![ProfileCandidate {
            value: String::new(),
            origin: ProfileOrigin::Explicit,
        }];
        // Empty-setting policy is unresolved; retain evidence rather than choose a reset.
        if let Some(value) = saved {
            candidates.push(ProfileCandidate {
                value,
                origin: ProfileOrigin::Retained,
            });
        } else if let Some((value, origin)) = default {
            candidates.push(ProfileCandidate { value, origin });
        }
        return ProfileValue::Pending {
            cause: PendingCause::OwnerPolicy,
            candidates,
        };
    }
    let value = pick(explicit, saved, default);
    if matches!(&value, ProfileValue::Selected(candidate) if candidate.value.is_empty()) {
        pending(value)
    } else {
        value
    }
}
fn platform_value(
    value: ProfileValue<bool>,
    platform: Option<ProfilePlatform>,
) -> ProfileValue<bool> {
    if platform == Some(ProfilePlatform::MacOs) {
        value
    } else {
        pending(value)
    }
}
