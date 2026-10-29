#![cfg(unix)]
#[path = "full_team_store_support/mod.rs"]
mod support;
use foundry_network::{
    code_invite::{CodePolicy, Codebook},
    full_team_store::*,
    team_state::Admission,
    ticket::TicketCodec,
};
use support::*;
fn codes(f: &Fixture) -> CodePolicy {
    let bytes = b"zircon\n";
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .try_into()
        .unwrap();
    CodePolicy::new(
        Codebook::from_bytes(bytes, digest).unwrap(),
        &f.base.profile,
    )
    .unwrap()
}
fn policy(f: &Fixture) -> StorePolicy {
    f.policy().with_codes(codes(f)).unwrap()
}
fn open(f: &Fixture) -> FullTeamStore {
    FullTeamStore::open_existing(&f.base.root, f.identity.clone(), policy(f), WAIT).unwrap()
}
fn formed(f: &Fixture) -> (FullTeamStore, LoadedTeam) {
    let mut store =
        FullTeamStore::create_new(&f.base.root, f.identity.clone(), policy(f), WAIT).unwrap();
    let base = store.load(NOW).unwrap();
    let pending = store.prepare_formation(&base, formation(), NOW).unwrap();
    let loaded = store.commit_formation(pending).unwrap();
    (store, loaded)
}
fn active(outcome: CommitOutcome) -> LoadedTeam {
    match outcome {
        CommitOutcome::Active(loaded) | CommitOutcome::Unchanged(loaded) => loaded,
        _ => panic!("unexpected removal"),
    }
}
#[test]
fn issue_is_atomic_private_and_recovers_with_same_ticket_secret() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let pending = store.prepare_code_intent(&base, 2, true, NOW).unwrap();
    assert!(store.recover_code_intents(NOW).unwrap().is_empty());
    assert!(store.code_effects(NOW).unwrap().is_empty());
    let issued = store.commit_code_intent(pending, NOW).unwrap();
    let ticket = TicketCodec::new(&f.base.profile, None)
        .unwrap()
        .decode(issued.ticket())
        .unwrap();
    let code = codes(&f).normalize(issued.code()).unwrap();
    let derived = codes(&f).derive(&code).unwrap();
    assert_eq!(ticket.secret, derived.bearer_secret());
    assert_eq!(ticket.peers, vec![f.identity.endpoint_id()]);
    let hash = data_encoding::HEXLOWER.encode(&derived.invitation_key());
    let invitation = &issued.loaded().document().unwrap().invites[&hash];
    assert_eq!(invitation.body.person.as_deref(), Some("alice"));
    assert_eq!(invitation.body.expires, NOW + 2 * 3_600_000);
    let id = issued.listener_intent().unwrap();
    let lease = store.recover_code_intents(NOW).unwrap().pop().unwrap();
    assert_eq!(lease.id(), id);
    assert_eq!(
        store
            .validate_code_lease(&lease, NOW)
            .unwrap()
            .endpoint_key()
            .public(),
        derived.endpoint_key().public()
    );
    let effects = store.code_effects(NOW).unwrap();
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, ControlEffect::ReconcileCode { id: i, .. } if *i == id))
    );
    let status = format!("{effects:?}");
    assert!(!status.contains(issued.code()));
    assert!(!status.contains(&derived.endpoint_key().public().to_string()));
    drop(issued);
    drop(store);
    let mut store = open(&f);
    assert!(matches!(
        store.validate_code_lease(&lease, NOW),
        Err(StoreError::Binding)
    ));
    let lease = store.recover_code_intents(NOW).unwrap().pop().unwrap();
    assert_eq!(lease.id(), id);
    assert_eq!(
        store
            .validate_code_lease(&lease, NOW)
            .unwrap()
            .bearer_secret(),
        ticket.secret
    );
}
#[test]
fn stale_cas_cannot_return_a_second_invitation() {
    let f = Fixture::new();
    let (mut a, base_a) = formed(&f);
    let mut b = open(&f);
    let base_b = b.load(NOW).unwrap();
    let one = a.prepare_code_intent(&base_a, 1, false, NOW).unwrap();
    let two = b.prepare_code_intent(&base_b, 1, false, NOW).unwrap();
    let committed = a.commit_code_intent(one, NOW).unwrap();
    assert!(matches!(
        b.commit_code_intent(two, NOW),
        Err(StoreError::Conflict)
    ));
    assert_eq!(
        b.load(NOW).unwrap().revision(),
        committed.loaded().revision()
    );
    assert_eq!(b.recover_code_intents(NOW).unwrap().len(), 1);
    assert!(matches!(
        a.prepare_code_intent(committed.loaded(), 1, false, NOW),
        Err(StoreError::Conflict)
    ));
}
#[test]
fn unrelated_revision_refreshes_generation_and_ack_is_a_real_mutation() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let pending = store.prepare_code_intent(&base, 1, false, NOW).unwrap();
    let issued = store.commit_code_intent(pending, NOW).unwrap();
    let old = store.recover_code_intents(NOW).unwrap().pop().unwrap();
    let stale_effect = store.code_effects(NOW).unwrap().pop().unwrap();
    let pending = store.prepare_rotation(issued.loaded(), NOW + 1).unwrap();
    let latest = active(store.commit_transition(pending, NOW + 1).unwrap());
    assert!(matches!(
        store.validate_code_lease(&old, NOW + 1),
        Err(StoreError::Conflict)
    ));
    let fresh = store.recover_code_intents(NOW + 1).unwrap().pop().unwrap();
    assert!(fresh.generation() > old.generation());
    assert_eq!(old.id(), fresh.id());
    assert!(matches!(
        store.prepare_effect_ack(&latest, &stale_effect, NOW + 1),
        Err(StoreError::Conflict)
    ));
    let effect = store.code_effects(NOW + 1).unwrap().pop().unwrap();
    let ack = store.prepare_effect_ack(&latest, &effect, NOW + 1).unwrap();
    let updated = active(store.commit_transition(ack, NOW + 1).unwrap());
    assert_eq!(updated.revision(), latest.revision() + 1);
    assert!(updated.document().unwrap() == latest.document().unwrap());
    assert!(store.validate_code_lease(&fresh, NOW + 1).is_ok());
    assert!(!store.code_effects(NOW + 1).unwrap().contains(&effect));
    drop(store);
    assert!(!open(&f).code_effects(NOW + 1).unwrap().contains(&effect));
}
#[test]
fn expiry_and_consumption_never_restart_a_code_listener() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let pending = store.prepare_code_intent(&base, 0, false, NOW).unwrap();
    let issued = store.commit_code_intent(pending, NOW).unwrap();
    let lease = store.recover_code_intents(NOW).unwrap().pop().unwrap();
    assert_eq!(lease.expires_ms(), NOW + 3_600_000);
    assert!(
        store
            .recover_code_intents(NOW + 3_600_000)
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        store.validate_code_lease(&lease, NOW + 3_600_000),
        Err(StoreError::Conflict)
    ));
    let ticket = TicketCodec::new(&f.base.profile, None)
        .unwrap()
        .decode(issued.ticket())
        .unwrap();
    let joiner = iroh::SecretKey::generate();
    let admission = || Admission {
        endpoint: joiner.public(),
        person: "bob",
        machine: "laptop",
        secret: &ticket.secret,
        proof: None,
        owner_only: false,
    };
    let pending = store
        .prepare_admission(issued.loaded(), admission(), NOW + 1)
        .unwrap();
    let latest = active(store.commit_transition(pending, NOW + 1).unwrap());
    assert!(store.recover_code_intents(NOW + 1).unwrap().is_empty());
    assert!(
        store
            .code_effects(NOW + 1)
            .unwrap()
            .iter()
            .any(|e| matches!(e, ControlEffect::RetireCode { .. }))
    );
    drop(store);
    let mut store = open(&f);
    assert!(store.recover_code_intents(NOW + 1).unwrap().is_empty());
    // Long-ticket retry remains available to the admitted active endpoint.
    let base = store.load(NOW + 1).unwrap();
    let pending = store
        .prepare_admission(&base, admission(), NOW + 2)
        .unwrap();
    store.commit_transition(pending, NOW + 2).unwrap();
    assert!(store.recover_code_intents(NOW + 2).unwrap().is_empty());
    assert!(
        latest
            .document()
            .unwrap()
            .members
            .contains_key(&joiner.public().to_string())
    );
}
#[test]
fn delayed_commit_and_local_departure_do_not_expose_or_recover_codes() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let pending = store.prepare_code_intent(&base, 1, false, NOW).unwrap();
    assert!(matches!(
        store.commit_code_intent(pending, NOW + 3_600_000),
        Err(StoreError::Invalid)
    ));
    assert_eq!(store.load(NOW).unwrap().revision(), base.revision());
    let pending = store.prepare_code_intent(&base, 1, false, NOW).unwrap();
    let issued = store.commit_code_intent(pending, NOW).unwrap();
    let code = issued.code().as_bytes().to_vec();
    let pending = store.prepare_departure(issued.loaded(), NOW + 1).unwrap();
    assert!(pending.requires_quiescence());
    assert!(matches!(
        store.commit_transition(pending, NOW + 1).unwrap(),
        CommitOutcome::Removed {
            scrub: ScrubStatus::Complete,
            ..
        }
    ));
    assert!(store.recover_code_intents(NOW + 1).unwrap().is_empty());
    assert!(store.code_effects(NOW + 1).unwrap().is_empty());
    drop(issued);
    drop(store);
    for file in std::fs::read_dir(&f.base.root).unwrap() {
        let path = file.unwrap().path();
        if path.is_file() {
            assert!(
                !std::fs::read(path)
                    .unwrap()
                    .windows(code.len())
                    .any(|w| w == code)
            );
        }
    }
    assert!(open(&f).recover_code_intents(NOW + 1).unwrap().is_empty());
}
#[test]
fn explicit_migration_preserves_long_invites_and_rejects_implicit_rebinding() {
    let f = Fixture::new();
    let mut store = f.create();
    let empty = store.load(NOW).unwrap();
    let pending = store.prepare_formation(&empty, formation(), NOW).unwrap();
    let base = store.commit_formation(pending).unwrap();
    assert!(matches!(
        store.prepare_code_intent(&base, 24, false, NOW),
        Err(StoreError::Unsupported)
    ));
    let pending = store
        .prepare_invite(&base, "preserved-long-secret", 24, false, NOW)
        .unwrap();
    let base = active(store.commit_transition(pending, NOW).unwrap());
    let before = serde_json::to_vec(base.document().unwrap()).unwrap();
    drop(store);
    assert!(matches!(
        FullTeamStore::open_existing(&f.base.root, f.identity.clone(), policy(&f), WAIT),
        Err(StoreError::Schema)
    ));
    let mut store = f.open().enable_codes(codes(&f), NOW + 1).unwrap();
    let after = store.load(NOW + 1).unwrap();
    assert_eq!(
        serde_json::to_vec(after.document().unwrap()).unwrap(),
        before
    );
    assert!(matches!(
        store.prepare_rotation(&base, NOW + 1),
        Err(StoreError::Binding)
    ));
    assert!(store.recover_code_intents(NOW + 1).unwrap().is_empty());
    let pending = store
        .prepare_code_intent(&after, 1, false, NOW + 1)
        .unwrap();
    store.commit_code_intent(pending, NOW + 1).unwrap();
    drop(store);
    assert_eq!(open(&f).recover_code_intents(NOW + 1).unwrap().len(), 1);
    assert!(matches!(
        FullTeamStore::open_existing(&f.base.root, f.identity.clone(), f.policy(), WAIT),
        Err(StoreError::Schema)
    ));
}
#[path = "full_team_store_intents_support/mod.rs"]
mod extra;
