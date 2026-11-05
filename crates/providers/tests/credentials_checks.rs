//! Deterministic, offline key validation and account checks.

#[path = "credentials_support.rs"]
mod support;

use foundry_identity::authz::Role;
use foundry_providers::credentials::{
    AccountCheck, AuthenticationStatus, CapacityStatus, CheckCause, CredentialError, EvidenceBasis,
    FixtureValidator, ValidationOutcome, validate,
};
use support::*;

const ONLINE: FixtureValidator = FixtureValidator { reachable: true };
const OFFLINE: FixtureValidator = FixtureValidator { reachable: false };

#[test]
fn fixture_validation_is_deterministic_and_saves_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path());
    let fixture = id("fixture");
    for _ in 0..3 {
        let valid = secret(FixtureValidator::VALID_KEY);
        assert_eq!(
            validate(&catalog(), &ONLINE, &fixture, &valid),
            Ok(ValidationOutcome::Verified)
        );
        let other = secret(CANARY);
        assert_eq!(
            validate(&catalog(), &ONLINE, &fixture, &other),
            Ok(ValidationOutcome::Rejected)
        );
    }
    // Same length as the valid key, one byte different.
    let near = secret("fixture-valie");
    assert_eq!(
        validate(&catalog(), &ONLINE, &fixture, &near),
        Ok(ValidationOutcome::Rejected)
    );

    let unverified = validate(
        &catalog(),
        &OFFLINE,
        &fixture,
        &secret(FixtureValidator::VALID_KEY),
    )
    .unwrap();
    assert_eq!(
        unverified,
        ValidationOutcome::Unverified(CheckCause::ProviderUnavailable)
    );
    assert!(!unverified.is_valid());
    assert_eq!(unverified.name(), "unverified");

    let keyless = validate(&catalog(), &ONLINE, &id("local-a"), &secret(CANARY));
    assert_eq!(keyless, Err(CredentialError::UnsupportedProvider));
    assert_eq!(
        validate(&catalog(), &ONLINE, &id("hosted-z"), &secret(CANARY))
            .unwrap_err()
            .class(),
        "not_found"
    );
    assert!(store.list(&owner()).unwrap().items.is_empty());
}

#[test]
fn check_reports_metadata_evidence_without_persisting_a_verdict() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path());
    store
        .configure(
            &owner(),
            &catalog(),
            request("fixture", None, FixtureValidator::VALID_KEY, &[]),
        )
        .unwrap();
    store
        .configure(&owner(), &catalog(), request("fixture", None, CANARY, &[]))
        .unwrap();
    let before = store.list(&owner()).unwrap();
    let check = |key: &str, validator: &FixtureValidator| {
        store
            .check_account(
                &owner(),
                &catalog(),
                validator,
                &id("fixture"),
                &account(key),
            )
            .unwrap()
    };

    let accepted = check("account-1", &ONLINE);
    assert_eq!(accepted.connected, Some(true));
    assert_eq!(accepted.observed_at_ms, NOW_MS);
    assert_eq!(AccountCheck::INFERENCE, "not_tested");
    let authentication = accepted.evidence.authentication;
    assert_eq!(authentication.status, AuthenticationStatus::Accepted);
    assert_eq!(authentication.basis, EvidenceBasis::ProviderMetadata);
    assert_eq!(authentication.cause.name(), "metadata_accepted");
    let capacity = accepted.evidence.capacity;
    assert_eq!(
        (capacity.status, capacity.basis),
        (CapacityStatus::Available, EvidenceBasis::KeyBudget)
    );
    assert_eq!(capacity.cause, CheckCause::KeyBudgetRemaining);

    let rejected = check("account-2", &ONLINE).evidence;
    assert_eq!(
        rejected.authentication.status,
        AuthenticationStatus::Rejected
    );
    assert_eq!(rejected.capacity.status, CapacityStatus::Unknown);
    let unreachable = check("account-1", &OFFLINE).evidence.authentication;
    assert_eq!(unreachable.status, AuthenticationStatus::Unverified);
    assert_eq!(unreachable.cause, CheckCause::ProviderUnavailable);
    assert_eq!(store.list(&owner()).unwrap(), before);
}

#[test]
fn check_reports_missing_and_unreadable_slots_as_states() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path());
    store
        .configure(&owner(), &catalog(), request("fixture", None, CANARY, &[]))
        .unwrap();

    let missing = store
        .check_account(
            &owner(),
            &catalog(),
            &ONLINE,
            &id("fixture"),
            &account("account-9"),
        )
        .unwrap();
    assert_eq!(missing.connected, Some(false));
    let authentication = missing.evidence.authentication;
    assert_eq!(authentication.status, AuthenticationStatus::Unverified);
    assert_eq!(
        (authentication.basis, authentication.cause),
        (EvidenceBasis::SavedCredential, CheckCause::NotConnected)
    );
    assert_eq!(missing.evidence.capacity.status, CapacityStatus::Unknown);
    assert_eq!(missing.evidence.capacity.basis, EvidenceBasis::Unknown);

    // Another workspace's slot is simply not connected here.
    let elsewhere = caller("workspace-b", "owner-b", Role::Owner);
    let isolated = store
        .check_account(
            &elsewhere,
            &catalog(),
            &ONLINE,
            &id("fixture"),
            &account("account-1"),
        )
        .unwrap();
    assert_eq!(isolated.connected, Some(false));

    let other_member = caller("workspace-a", "member-b", Role::Member);
    let forbidden = store.check_account(
        &other_member,
        &catalog(),
        &ONLINE,
        &id("fixture"),
        &account("account-1"),
    );
    assert_eq!(forbidden, Err(CredentialError::Forbidden));
    let unknown = store.check_account(
        &owner(),
        &catalog(),
        &ONLINE,
        &id("hosted-z"),
        &account("account-1"),
    );
    assert_eq!(unknown.unwrap_err().class(), "not_found");

    // Storage that cannot be read reports `connected: None`, not an error.
    let saboteur = rusqlite::Connection::open(path(directory.path())).unwrap();
    saboteur
        .execute_batch("DROP TABLE credential_config; DROP TABLE credential_slots;")
        .unwrap();
    let unreadable = store
        .check_account(
            &owner(),
            &catalog(),
            &ONLINE,
            &id("fixture"),
            &account("account-1"),
        )
        .unwrap();
    assert_eq!(unreadable.connected, None);
    assert_eq!(
        unreadable.evidence.authentication.cause,
        CheckCause::StorageUnavailable
    );
    assert_eq!(store.list(&owner()).unwrap_err().class(), "unavailable");
}
