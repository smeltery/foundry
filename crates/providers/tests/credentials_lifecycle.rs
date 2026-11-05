//! Credential lifecycle: configure, list, replace, provider events and delete.

#[path = "credentials_support.rs"]
mod support;

use foundry_identity::authz::WorkspaceId;
use foundry_providers::catalog::{AuthKind, ProviderId};
use foundry_providers::credentials::{
    AccountKey, CredentialError, CredentialEvent, CredentialState, Failure, MAX_SECRET_BYTES,
    Secret,
};
use support::*;

#[test]
fn configure_stores_key_and_returns_only_names() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path());
    let configured = store
        .configure(
            &owner(),
            &catalog(),
            request("hosted-a", None, CANARY, &[("region", "north")]),
        )
        .unwrap();
    assert_eq!(configured.account_key, account("account-1"));
    assert_eq!(configured.config_names, ["region"]);
    assert_eq!(configured.kind, AuthKind::ApiKey);
    assert_eq!(configured.state, CredentialState::Configured);

    let list = store.list(&owner()).unwrap();
    assert!(list.readiness.store_available && list.readiness.configure);
    let slot = &list.items[0];
    assert_eq!(slot.config_names, ["region"]);
    assert_eq!(slot.connected_by.as_str(), "owner-a");
    assert_eq!((slot.created_at_ms, slot.updated_at_ms), (NOW_MS, NOW_MS));
    assert_eq!(slot.failure, None);
}

#[test]
fn several_accounts_per_provider_list_in_provider_then_account_order() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path());
    let slots = [
        ("hosted-a", None),
        ("fixture", Some("zeta")),
        ("hosted-a", None),
        ("fixture", Some("alpha")),
    ];
    for (provider_id, key) in slots {
        store
            .configure(&owner(), &catalog(), request(provider_id, key, CANARY, &[]))
            .unwrap();
    }
    let expected = pairs(&[
        ("fixture", "alpha"),
        ("fixture", "zeta"),
        ("hosted-a", "account-1"),
        ("hosted-a", "account-2"),
    ]);
    assert_eq!(keys(&store, &owner()), expected);

    // Naming the slot replaces it in place, so a repeat is idempotent.
    for _ in 0..2 {
        let named = request("hosted-a", Some("account-1"), OTHER_CANARY, &[]);
        store.configure(&owner(), &catalog(), named).unwrap();
    }
    assert_eq!(keys(&store, &owner()), expected);
    assert_eq!(
        stored_value(&store, &owner(), "hosted-a", "account-1"),
        OTHER_CANARY
    );
    assert_eq!(
        stored_value(&store, &owner(), "hosted-a", "account-2"),
        CANARY
    );
}

#[test]
fn config_is_checked_against_and_kept_with_its_own_provider() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path());
    let invalid = Err(CredentialError::InvalidRequest { field: "config" });
    let refused: [&[(&str, &str)]; 4] = [
        &[("undeclared", "x")],
        &[("tier", "gold")],
        &[("region", "")],
        &[("region", "a\nb")],
    ];
    for config in refused {
        let result = store.configure(
            &owner(),
            &catalog(),
            request("hosted-a", None, CANARY, config),
        );
        assert_eq!(result.map(|_| ()), invalid);
    }
    // `tier` is required by hosted-b and belongs to it alone.
    let missing = store.configure(&owner(), &catalog(), request("hosted-b", None, CANARY, &[]));
    assert_eq!(missing.map(|_| ()), invalid);
    store
        .configure(
            &owner(),
            &catalog(),
            request("hosted-b", None, CANARY, &[("tier", "gold")]),
        )
        .unwrap();
    store
        .configure(
            &owner(),
            &catalog(),
            request("hosted-a", None, CANARY, &[("region", "north")]),
        )
        .unwrap();
    let names: Vec<_> = store
        .list(&owner())
        .unwrap()
        .items
        .into_iter()
        .map(|slot| slot.config_names)
        .collect();
    assert_eq!(names, [vec!["region".to_owned()], vec!["tier".to_owned()]]);

    // Replacing a slot replaces its whole config.
    store
        .configure(
            &owner(),
            &catalog(),
            request("hosted-a", Some("account-1"), CANARY, &[]),
        )
        .unwrap();
    assert!(
        store.list(&owner()).unwrap().items[0]
            .config_names
            .is_empty()
    );
}

#[test]
fn malformed_requests_are_refused_by_class() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path());
    let unknown = store.configure(&owner(), &catalog(), request("hosted-z", None, CANARY, &[]));
    assert_eq!(unknown.unwrap_err().class(), "not_found");
    let keyless = store.configure(&owner(), &catalog(), request("local-a", None, CANARY, &[]));
    assert_eq!(keyless.unwrap_err(), CredentialError::UnsupportedProvider);

    assert_eq!(
        Secret::new(String::new()).unwrap_err().class(),
        "invalid_request"
    );
    assert_eq!(
        Secret::new("k".repeat(MAX_SECRET_BYTES + 1))
            .unwrap_err()
            .class(),
        "invalid_request"
    );
    assert!(Secret::new("k".repeat(MAX_SECRET_BYTES)).is_ok());
    assert_eq!(
        AccountKey::new("").unwrap_err(),
        CredentialError::InvalidRequest {
            field: "account_key"
        }
    );
    assert!(AccountKey::new(&"é".repeat(512)).is_ok());
    assert!(AccountKey::new(&"a".repeat(513)).is_err());
    assert!(ProviderId::new(&"a".repeat(65)).is_err());
    assert!(store.list(&owner()).unwrap().items.is_empty());
}

#[test]
fn provider_events_follow_the_state_table() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path());
    let workspace = WorkspaceId::new("workspace-a");
    let (provider_id, key) = (id("hosted-a"), account("account-1"));
    let conflict = CredentialError::Conflict {
        what: "credential state",
    };
    store
        .configure(&owner(), &catalog(), request("hosted-a", None, CANARY, &[]))
        .unwrap();

    let refused = store.record(
        &workspace,
        &provider_id,
        &key,
        CredentialEvent::ProviderRefused,
    );
    assert_eq!(refused, Ok(CredentialState::AuthFailed));
    let slot = store.list(&owner()).unwrap().items.remove(0);
    assert_eq!(
        slot.failure,
        Some(Failure {
            code: "provider_refused",
            at_ms: NOW_MS
        })
    );
    for event in [
        CredentialEvent::ProviderRefused,
        CredentialEvent::TokenExpired,
    ] {
        assert_eq!(
            store.record(&workspace, &provider_id, &key, event),
            Err(conflict)
        );
    }
    assert_eq!(
        store.handoff(&owner(), &provider_id, &key).unwrap_err(),
        conflict
    );
    let available = store.availability(&workspace).unwrap();
    assert!(
        !catalog()
            .provider_available(&provider_id, &available)
            .unwrap()
    );

    let replace = request("hosted-a", Some("account-1"), CANARY, &[]);
    store.configure(&owner(), &catalog(), replace).unwrap();
    let slot = store.list(&owner()).unwrap().items.remove(0);
    assert_eq!(
        (slot.state, slot.failure),
        (CredentialState::Configured, None)
    );
    let available = store.availability(&workspace).unwrap();
    assert!(
        catalog()
            .provider_available(&provider_id, &available)
            .unwrap()
    );

    let expired = store.record(
        &workspace,
        &provider_id,
        &key,
        CredentialEvent::TokenExpired,
    );
    assert_eq!(expired, Ok(CredentialState::ReconnectRequired));
    let deleted = store.delete(&owner(), &provider_id, Some(&key)).unwrap();
    assert_eq!(deleted.account_keys, std::slice::from_ref(&key));
    let absent = store.record(
        &workspace,
        &provider_id,
        &key,
        CredentialEvent::ProviderRefused,
    );
    assert_eq!(absent.unwrap_err().class(), "not_found");
}

#[test]
fn delete_targets_one_slot_or_every_slot_of_the_provider() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path());
    for provider_id in ["hosted-a", "hosted-a", "hosted-a", "fixture"] {
        store
            .configure(
                &owner(),
                &catalog(),
                request(provider_id, None, CANARY, &[]),
            )
            .unwrap();
    }
    let one = store
        .delete(&owner(), &id("hosted-a"), Some(&account("account-1")))
        .unwrap();
    assert_eq!(one.account_keys, [account("account-1")]);
    let missing = store.delete(&owner(), &id("hosted-a"), Some(&account("account-9")));
    assert_eq!(
        missing.unwrap_err(),
        CredentialError::NotFound { what: "credential" }
    );
    // A running attempt is not cancelled, but its next handoff finds nothing.
    let gone = store.handoff(&owner(), &id("hosted-a"), &account("account-1"));
    assert_eq!(gone.unwrap_err().class(), "not_found");

    let rest = store.delete(&owner(), &id("hosted-a"), None).unwrap();
    assert_eq!(
        rest.account_keys,
        [account("account-2"), account("account-3")]
    );
    assert!(rest.secret_purged);
    assert_eq!(keys(&store, &owner()), pairs(&[("fixture", "account-1")]));
    assert_eq!(
        store
            .delete(&owner(), &id("hosted-a"), None)
            .unwrap_err()
            .class(),
        "not_found"
    );
}

#[test]
fn slots_survive_reopening_the_file() {
    let directory = tempfile::tempdir().unwrap();
    let main = request("hosted-b", Some("main"), CANARY, &[("tier", "gold")]);
    open(directory.path())
        .configure(&owner(), &catalog(), main)
        .unwrap();
    let store = open(directory.path());
    let slot = store.list(&owner()).unwrap().items.remove(0);
    assert_eq!(slot.account_key.as_str(), "main");
    assert_eq!(slot.config_names, ["tier"]);
    assert_eq!(stored_value(&store, &owner(), "hosted-b", "main"), CANARY);
}
