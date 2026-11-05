//! Who may change a slot: roles, ownership and workspace isolation.

#[path = "credentials_support.rs"]
mod support;

use foundry_identity::authz::{Role, WorkspaceId};
use foundry_providers::credentials::{CredentialError, CredentialEvent, CredentialState};
use support::*;

#[test]
fn members_manage_only_slots_they_connected() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path());
    let member_b = caller("workspace-a", "member-b", Role::Member);
    let member_c = caller("workspace-a", "member-c", Role::Member);
    let admin = caller("workspace-a", "admin-a", Role::Admin);
    let viewer = caller("workspace-a", "viewer-a", Role::Viewer);
    store
        .configure(
            &member_b,
            &catalog(),
            request("hosted-a", Some("account-1"), CANARY, &[]),
        )
        .unwrap();

    let forbidden = CredentialError::Forbidden;
    let replace = || request("hosted-a", Some("account-1"), OTHER_CANARY, &[]);
    assert_eq!(
        store
            .configure(&member_c, &catalog(), replace())
            .unwrap_err(),
        forbidden
    );
    let fresh = request("fixture", None, CANARY, &[]);
    assert_eq!(
        store.configure(&viewer, &catalog(), fresh).unwrap_err(),
        forbidden
    );
    assert_eq!(
        store.delete(&member_c, &id("hosted-a"), None).unwrap_err(),
        forbidden
    );
    let handoff = store.handoff(&member_c, &id("hosted-a"), &account("account-1"));
    assert_eq!(handoff.unwrap_err(), forbidden);
    assert_eq!(
        stored_value(&store, &member_b, "hosted-a", "account-1"),
        CANARY
    );
    let viewed = store.list(&viewer).unwrap();
    assert!(!viewed.readiness.configure && !viewed.readiness.delete);
    assert_eq!(viewed.items.len(), 1);

    // The administrator's replacement makes the slot theirs.
    store.configure(&admin, &catalog(), replace()).unwrap();
    assert_eq!(
        store.list(&admin).unwrap().items[0].connected_by.as_str(),
        "admin-a"
    );
    assert_eq!(
        store.delete(&member_b, &id("hosted-a"), None).unwrap_err(),
        forbidden
    );
    assert_eq!(
        stored_value(&store, &admin, "hosted-a", "account-1"),
        OTHER_CANARY
    );
}

#[test]
fn workspaces_never_see_or_change_each_others_slots() {
    let directory = tempfile::tempdir().unwrap();
    let store = open(directory.path());
    let other = caller("workspace-b", "owner-b", Role::Owner);
    let north = request("hosted-a", None, CANARY, &[("region", "north")]);
    store.configure(&owner(), &catalog(), north).unwrap();
    store
        .configure(
            &other,
            &catalog(),
            request("hosted-a", None, OTHER_CANARY, &[]),
        )
        .unwrap();
    store
        .configure(
            &other,
            &catalog(),
            request("fixture", None, OTHER_CANARY, &[]),
        )
        .unwrap();

    // Both workspaces allocate account-1 independently.
    assert_eq!(keys(&store, &owner()), pairs(&[("hosted-a", "account-1")]));
    assert_eq!(keys(&store, &other).len(), 2);
    assert!(
        store
            .list(&other)
            .unwrap()
            .items
            .iter()
            .all(|slot| slot.config_names.is_empty())
    );
    assert_eq!(
        stored_value(&store, &owner(), "hosted-a", "account-1"),
        CANARY
    );
    assert_eq!(
        stored_value(&store, &other, "hosted-a", "account-1"),
        OTHER_CANARY
    );
    let foreign = store.handoff(&owner(), &id("fixture"), &account("account-1"));
    assert_eq!(foreign.unwrap_err().class(), "not_found");
    assert_eq!(
        store
            .delete(&owner(), &id("fixture"), None)
            .unwrap_err()
            .class(),
        "not_found"
    );

    // A same-named principal in another workspace has no standing here.
    let impostor = caller("workspace-b", "owner-a", Role::Member);
    let refused = store.delete(&impostor, &id("hosted-a"), None);
    assert_eq!(refused.unwrap_err(), CredentialError::Forbidden);
    let ws_a = WorkspaceId::new("workspace-a");
    let event = CredentialEvent::ProviderRefused;
    let recorded = store.record(&ws_a, &id("hosted-a"), &account("account-1"), event);
    assert_eq!(recorded, Ok(CredentialState::AuthFailed));
    assert_eq!(
        store.list(&other).unwrap().items[1].state,
        CredentialState::Configured
    );

    store.delete(&owner(), &id("hosted-a"), None).unwrap();
    assert!(store.list(&owner()).unwrap().items.is_empty());
    assert_eq!(
        stored_value(&store, &other, "hosted-a", "account-1"),
        OTHER_CANARY
    );
    let fixture = id("fixture");
    let in_a = store.availability(&ws_a).unwrap();
    assert!(!catalog().provider_available(&fixture, &in_a).unwrap());
    let in_b = store
        .availability(&WorkspaceId::new("workspace-b"))
        .unwrap();
    assert!(catalog().provider_available(&fixture, &in_b).unwrap());
}
