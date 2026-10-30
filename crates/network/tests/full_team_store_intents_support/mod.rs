use super::*;
#[test]
fn policy_binding_is_to_actual_order_and_explicit_contexts() {
    let f = Fixture::new();
    let (store, _) = formed(&f);
    drop(store);
    let bytes = b"topaz\n";
    let hash = ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .try_into()
        .unwrap();
    let other =
        CodePolicy::new(Codebook::from_bytes(bytes, hash).unwrap(), &f.base.profile).unwrap();
    let wrong = f.policy().with_codes(other).unwrap();
    assert!(matches!(
        FullTeamStore::open_existing(&f.base.root, f.identity.clone(), wrong, WAIT),
        Err(StoreError::Binding)
    ));
    let mut profile = f.base.profile.clone();
    profile.invitation_secret_context.push_str(" changed");
    let mismatched = StorePolicy::new(profile, None)
        .unwrap()
        .with_codes(codes(&f));
    assert!(matches!(mismatched, Err(StoreError::Binding)));
    assert!(open(&f).load(NOW).unwrap().document().is_some());
}
#[test]
fn empty_migration_retains_empty_semantics() {
    let f = Fixture::new();
    let store = f.create();
    let mut store = store.enable_codes(codes(&f), NOW).unwrap();
    let empty = store.load(NOW).unwrap();
    assert_eq!(empty.revision(), 0);
    assert!(empty.document().is_none());
    let pending = store.prepare_formation(&empty, formation(), NOW).unwrap();
    let base = store.commit_formation(pending).unwrap();
    let pending = store.prepare_code_intent(&base, 1, false, NOW).unwrap();
    store.commit_code_intent(pending, NOW).unwrap();
    assert_eq!(store.recover_code_intents(NOW).unwrap().len(), 1);
}
#[test]
fn stale_effect_ack_cas_preserves_newer_descriptors() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let pending = store.prepare_code_intent(&base, 1, false, NOW).unwrap();
    let issued = store.commit_code_intent(pending, NOW).unwrap();
    let effect = store.code_effects(NOW).unwrap().pop().unwrap();
    let ack = store
        .prepare_effect_ack(issued.loaded(), &effect, NOW)
        .unwrap();
    let update = store.prepare_rotation(issued.loaded(), NOW + 1).unwrap();
    store.commit_transition(update, NOW + 1).unwrap();
    let effects = store.code_effects(NOW + 1).unwrap();
    assert!(matches!(
        store.commit_transition(ack, NOW + 1),
        Err(StoreError::Conflict)
    ));
    assert_eq!(store.code_effects(NOW + 1).unwrap(), effects);
}
