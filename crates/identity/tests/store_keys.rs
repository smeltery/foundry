//! Scoped keys and logins: issue, list, revoke, logout and expiry.
use std::sync::Arc;
use std::time::Duration;

use foundry_identity::authz::{CredentialKind, CredentialState, PrincipalId, Scope, WorkspaceId};
use foundry_identity::store::{CredentialStore, ManualClock, StoreError};

const HOUR: Duration = Duration::from_secs(3600);

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::new(id)
}

fn who(id: &str) -> PrincipalId {
    PrincipalId::new(id)
}

fn store() -> (CredentialStore, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::default());
    (
        CredentialStore::open_in_memory(clock.clone()).unwrap(),
        clock,
    )
}

#[test]
fn issued_key_authenticates_as_its_service_principal_and_workspace() {
    let (store, _) = store();
    let issued = store
        .issue_key(&who("svc-a"), &ws("ws-a"), [Scope::WorkspaceRead])
        .unwrap();
    let principal = store.authenticate(issued.secret.expose()).unwrap();
    assert_eq!(principal.id, who("svc-a"));
    assert_eq!(principal.credential.kind, CredentialKind::ScopedKey);
    assert_eq!(principal.credential.state, CredentialState::Active);
    assert_eq!(principal.credential.workspace, ws("ws-a"));
    assert!(principal.credential.grants(Scope::WorkspaceRead));
    assert!(!principal.credential.grants(Scope::WorkspaceWrite));
    assert_eq!(issued.record.expires_ms, None);
}

#[test]
fn listing_shows_the_display_prefix_and_state_within_one_workspace() {
    let (store, clock) = store();
    // `first` and `second` share a millisecond, so issue order decides; `third`
    // is later, so its time does.
    let first = store.issue_key(&who("svc-a"), &ws("ws-a"), []).unwrap();
    let second = store.issue_key(&who("svc-b"), &ws("ws-a"), []).unwrap();
    clock.advance(5);
    let third = store.issue_key(&who("svc-c"), &ws("ws-a"), []).unwrap();
    store.issue_key(&who("svc-d"), &ws("ws-b"), []).unwrap();
    store
        .issue_login(&who("user-a"), &ws("ws-a"), [], HOUR)
        .unwrap();
    assert!(store.revoke_key(&ws("ws-a"), &first.record.prefix).unwrap());

    let keys = store.list_keys(&ws("ws-a")).unwrap();
    let listed: Vec<_> = keys
        .iter()
        .map(|key| (&key.prefix, key.state, key.created_ms))
        .collect();
    assert_eq!(
        listed,
        [
            (&first.record.prefix, CredentialState::Revoked, 0),
            (&second.record.prefix, CredentialState::Active, 0),
            (&third.record.prefix, CredentialState::Active, 5),
        ]
    );
    assert!(first.record.prefix.starts_with("key_"));
    let presented = first.secret.expose();
    assert!(presented.starts_with(&format!("{}_", first.record.prefix)));
    let secret_part = presented.rsplit_once('_').unwrap().1;
    assert!(!format!("{keys:?}").contains(secret_part));
}

#[test]
fn revoked_key_is_rejected_on_the_next_request_and_others_stay_valid() {
    let (store, _) = store();
    let revoked = store.issue_key(&who("svc-a"), &ws("ws-a"), []).unwrap();
    let kept = store.issue_key(&who("svc-a"), &ws("ws-a"), []).unwrap();
    store.authenticate(revoked.secret.expose()).unwrap();

    assert!(
        store
            .revoke_key(&ws("ws-a"), &revoked.record.prefix)
            .unwrap()
    );
    assert_eq!(
        store.authenticate(revoked.secret.expose()),
        Err(StoreError::Unauthenticated)
    );
    assert!(store.authenticate(kept.secret.expose()).is_ok());
    assert!(
        !store
            .revoke_key(&ws("ws-a"), &revoked.record.prefix)
            .unwrap()
    );
}

#[test]
fn a_key_outside_the_workspace_or_a_login_cannot_be_revoked_as_a_key() {
    let (store, _) = store();
    let key = store.issue_key(&who("svc-a"), &ws("ws-a"), []).unwrap();
    let login = store
        .issue_login(&who("user-a"), &ws("ws-a"), [], HOUR)
        .unwrap();
    let not_found = Err(StoreError::NotFound { what: "key" });
    assert_eq!(store.revoke_key(&ws("ws-b"), &key.record.prefix), not_found);
    assert_eq!(
        store.revoke_key(&ws("ws-a"), &login.record.prefix),
        not_found
    );
    assert_eq!(store.revoke_key(&ws("ws-a"), "key_UNKNOWN234"), not_found);
    assert!(store.authenticate(key.secret.expose()).is_ok());
    assert!(store.authenticate(login.secret.expose()).is_ok());
}

#[test]
fn altered_unknown_and_malformed_text_is_unauthenticated() {
    let (store, _) = store();
    let issued = store.issue_key(&who("svc-a"), &ws("ws-a"), []).unwrap();
    let presented = issued.secret.expose();
    let last = presented.chars().last().unwrap();
    let altered = format!(
        "{}{}",
        &presented[..presented.len() - 1],
        if last == 'A' { 'B' } else { 'A' }
    );
    let as_login = presented.replacen("key_", "login_", 1);
    let lowercase = presented.to_lowercase();
    let truncated = &presented[..presented.len() - 1];
    for text in [
        altered.as_str(),
        as_login.as_str(),
        lowercase.as_str(),
        truncated,
        "",
        "key__",
        "ABCDEFGH-JKLMNPQR",
    ] {
        assert_eq!(store.authenticate(text), Err(StoreError::Unauthenticated));
    }
}

#[test]
fn login_is_active_until_its_lifetime_elapses() {
    let (store, clock) = store();
    let login = store
        .issue_login(&who("user-a"), &ws("ws-a"), [Scope::SessionsRun], HOUR)
        .unwrap();
    assert_eq!(login.record.expires_ms, Some(3_600_000));
    let principal = store.authenticate(login.secret.expose()).unwrap();
    assert_eq!(principal.credential.kind, CredentialKind::LoginGrant);
    assert!(principal.credential.grants(Scope::SessionsRun));

    clock.advance(3_599_999);
    assert!(store.authenticate(login.secret.expose()).is_ok());
    clock.advance(1);
    assert_eq!(
        store.authenticate(login.secret.expose()),
        Err(StoreError::Unauthenticated)
    );
    assert!(!store.logout(login.secret.expose()).unwrap());
}

#[test]
fn zero_or_unrepresentable_lifetime_is_invalid() {
    let (store, _) = store();
    for lifetime in [Duration::ZERO, Duration::MAX] {
        assert_eq!(
            store
                .issue_login(&who("user-a"), &ws("ws-a"), [], lifetime)
                .unwrap_err(),
            StoreError::InvalidRequest { field: "lifetime" }
        );
    }
    assert_eq!(
        store.issue_key(&who(""), &ws("ws-a"), []).unwrap_err(),
        StoreError::InvalidRequest { field: "principal" }
    );
    assert_eq!(
        store.issue_key(&who("svc-a"), &ws(""), []).unwrap_err(),
        StoreError::InvalidRequest { field: "workspace" }
    );
}

#[test]
fn logout_revokes_only_the_callers_own_login() {
    let (store, _) = store();
    let own = store
        .issue_login(&who("user-a"), &ws("ws-a"), [], HOUR)
        .unwrap();
    let other = store
        .issue_login(&who("user-b"), &ws("ws-a"), [], HOUR)
        .unwrap();
    let key = store.issue_key(&who("svc-a"), &ws("ws-a"), []).unwrap();

    assert!(store.logout(own.secret.expose()).unwrap());
    assert_eq!(
        store.authenticate(own.secret.expose()),
        Err(StoreError::Unauthenticated)
    );
    assert!(store.authenticate(other.secret.expose()).is_ok());
    assert!(store.authenticate(key.secret.expose()).is_ok());
    assert!(!store.logout(own.secret.expose()).unwrap());
    assert_eq!(
        store.logout(key.secret.expose()),
        Err(StoreError::Unauthenticated)
    );
}

#[test]
fn member_removal_revokes_that_users_logins_in_one_workspace() {
    let (store, _) = store();
    let first = store
        .issue_login(&who("user-a"), &ws("ws-a"), [], HOUR)
        .unwrap();
    let second = store
        .issue_login(&who("user-a"), &ws("ws-a"), [], HOUR)
        .unwrap();
    let elsewhere = store
        .issue_login(&who("user-a"), &ws("ws-b"), [], HOUR)
        .unwrap();
    let colleague = store
        .issue_login(&who("user-b"), &ws("ws-a"), [], HOUR)
        .unwrap();

    assert_eq!(store.revoke_logins(&who("user-a"), &ws("ws-a")).unwrap(), 2);
    for revoked in [&first, &second] {
        assert_eq!(
            store.authenticate(revoked.secret.expose()),
            Err(StoreError::Unauthenticated)
        );
    }
    assert!(store.authenticate(elsewhere.secret.expose()).is_ok());
    assert!(store.authenticate(colleague.secret.expose()).is_ok());
    assert_eq!(store.revoke_logins(&who("user-a"), &ws("ws-a")).unwrap(), 0);
}
