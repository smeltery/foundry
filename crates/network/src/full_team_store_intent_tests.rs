use super::tests::Fixture;
use super::*;
const NOW: i64 = 1_000_000;
fn codes(profile: &WireProfile) -> crate::code_invite::CodePolicy {
    let bytes = b"zircon\n";
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .try_into()
        .unwrap();
    crate::code_invite::CodePolicy::new(
        crate::code_invite::Codebook::from_bytes(bytes, digest).unwrap(),
        profile,
    )
    .unwrap()
}
fn formed(f: &Fixture) -> (FullTeamStore, LoadedTeam) {
    let mut store = FullTeamStore::create_new(
        &f.root,
        f.identity.clone(),
        f.policy().with_codes(codes(&f.profile)).unwrap(),
        Duration::from_secs(1),
    )
    .unwrap();
    let base = store.load(NOW).unwrap();
    let pending = store
        .prepare_formation(
            &base,
            Formation {
                name: "group",
                person: "alice",
                machine: "desk",
            },
            NOW,
        )
        .unwrap();
    let base = store.commit_formation(pending).unwrap();
    (store, base)
}
fn reopen(f: &Fixture) -> FullTeamStore {
    FullTeamStore::open_existing(
        &f.root,
        f.identity.clone(),
        f.policy().with_codes(codes(&f.profile)).unwrap(),
        Duration::from_secs(1),
    )
    .unwrap()
}
#[test]
fn sync_failure_never_returns_secret_but_recovery_finds_atomic_commit() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let pending = store.prepare_code_intent(&base, 1, false, NOW).unwrap();
    store.fail_sync = true;
    assert!(matches!(
        store.commit_code_intent(pending, NOW),
        Err(StoreError::Indeterminate)
    ));
    drop(store);
    let mut store = reopen(&f);
    let loaded = store.load(NOW).unwrap();
    assert_eq!(loaded.document().unwrap().invites.len(), 2);
    assert_eq!(store.recover_code_intents(NOW).unwrap().len(), 1);
    assert_eq!(store.code_effects(NOW).unwrap().len(), 2);
}
#[test]
fn sealed_association_corruption_is_an_error_not_filtered_recovery() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let pending = store.prepare_code_intent(&base, 1, false, NOW).unwrap();
    store.commit_code_intent(pending, NOW).unwrap();
    let (original, _) =
        checkpoint::read(&store.connection, &store.identity, &store.policy).unwrap();
    let bytes = checkpoint::encode(&original).unwrap();
    for kind in 0..9 {
        let mut altered: Checkpoint = serde_json::from_slice(&bytes).unwrap();
        match kind {
            0 => altered.code_intents[0].invitation_hash = "00".repeat(32),
            1 => altered.code_intents[0].code = "unknown-unknown-unknown-unknown".into(),
            2 => altered.code_intents[0].policy[0] ^= 1,
            3 => altered.code_intents[0].incarnation[0] ^= 1,
            4 => altered.code_intents[0].team.push('x'),
            5 => altered.code_intents[0].expires_ms += 3_600_000,
            6 => altered.code_intents.push(altered.code_intents[0].clone()),
            7 => altered.network_generation += 1,
            _ => altered.effects.push(altered.effects[0].clone()),
        }
        checkpoint::write(&store.connection, &altered, &store.identity, false).unwrap();
        assert!(matches!(
            store.recover_code_intents(NOW),
            Err(StoreError::Corrupt)
        ));
    }
    checkpoint::write(&store.connection, &original, &store.identity, false).unwrap();
    assert_eq!(store.recover_code_intents(NOW).unwrap().len(), 1);
}
#[test]
fn private_relay_uses_same_long_ticket_without_listener_intent() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let mut document = base.document().unwrap().clone();
    let mut metadata = document.meta.body.clone();
    metadata.relay = Some("https://relay.invalid".into());
    metadata.ts = NOW + 1;
    document.meta = crate::signing::SignedRecord::sign(
        metadata,
        f.identity.secret_key(),
        &document.id,
        &f.profile,
    )
    .unwrap();
    let (pending, _) = store
        .prepare_merge(&base, f.identity.endpoint_id(), &document, NOW + 1)
        .unwrap();
    let base = match store.commit_transition(pending, NOW + 1).unwrap() {
        CommitOutcome::Active(base) => base,
        _ => panic!("not active"),
    };
    let pending = store.prepare_code_intent(&base, 1, false, NOW + 1).unwrap();
    let issued = store.commit_code_intent(pending, NOW + 1).unwrap();
    assert_eq!(issued.code(), issued.ticket());
    assert!(issued.listener_intent().is_none());
    let ticket = crate::ticket::TicketCodec::new(&f.profile, None)
        .unwrap()
        .decode(issued.ticket())
        .unwrap();
    let canonical = codes(&f.profile)
        .normalize("zircon-zircon-zircon-zircon")
        .unwrap();
    assert_eq!(
        ticket.secret,
        codes(&f.profile)
            .derive(&canonical)
            .unwrap()
            .bearer_secret()
    );
    assert_eq!(ticket.relay.as_deref(), Some("https://relay.invalid"));
    assert!(store.recover_code_intents(NOW + 1).unwrap().is_empty());
    drop(store);
    assert!(reopen(&f).recover_code_intents(NOW + 1).unwrap().is_empty());
}
#[test]
fn revocation_candidate_never_encodes_intent_or_replacement_secrets() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let pending = store.prepare_code_intent(&base, 1, false, NOW).unwrap();
    let issued = store.commit_code_intent(pending, NOW).unwrap();
    let lease = store.recover_code_intents(NOW).unwrap().pop().unwrap();
    let derived = store.validate_code_lease(&lease, NOW).unwrap();
    let probe = checkpoint::forbid_encoding(vec![
        issued.code().as_bytes().to_vec(),
        derived.bearer_secret().as_bytes().to_vec(),
        derived.endpoint_key().public().to_string().into_bytes(),
    ]);
    let pending = store.prepare_departure(issued.loaded(), NOW + 1).unwrap();
    assert!(pending.requires_quiescence());
    checkpoint::encode(&pending.checkpoint).unwrap();
    // Commit must canonicalize the previously sealed base for CAS. That base
    // intentionally still contains the old code; only the new candidate is scrubbed.
    drop(probe);
    assert!(matches!(
        store.commit_transition(pending, NOW + 1).unwrap(),
        CommitOutcome::Removed { .. }
    ));
}
#[test]
fn migration_sync_failure_is_recoverable_as_v3_without_losing_document() {
    let f = Fixture::new();
    let mut store = f.store();
    let empty = store.load(NOW).unwrap();
    let pending = store
        .prepare_formation(
            &empty,
            Formation {
                name: "group",
                person: "alice",
                machine: "desk",
            },
            NOW,
        )
        .unwrap();
    let base = store.commit_formation(pending).unwrap();
    let before = checkpoint::encode(base.document().unwrap()).unwrap();
    store.fail_sync = true;
    assert!(matches!(
        store.enable_codes(codes(&f.profile), NOW),
        Err(StoreError::Indeterminate)
    ));
    let mut store = reopen(&f);
    assert_eq!(
        checkpoint::encode(store.load(NOW).unwrap().document().unwrap()).unwrap(),
        before
    );
}
#[test]
fn crash_child() {
    let Some(root) = std::env::var_os("FOUNDRY_INTENT_CRASH_ROOT") else {
        return;
    };
    let profile: WireProfile =
        serde_json::from_str(&std::env::var("FOUNDRY_INTENT_CRASH_PROFILE").unwrap()).unwrap();
    let root = std::path::PathBuf::from(root);
    let directory =
        cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
    let identity =
        Arc::new(crate::load_or_create_identity(&directory, Duration::from_secs(1)).unwrap());
    let policy = StorePolicy::new(profile.clone(), None)
        .unwrap()
        .with_codes(codes(&profile))
        .unwrap();
    let mut store =
        FullTeamStore::open_existing(&root, identity, policy, Duration::from_secs(1)).unwrap();
    let base = store.load(NOW).unwrap();
    let pending = store.prepare_code_intent(&base, 1, false, NOW).unwrap();
    store.crash_after_commit = true;
    let _ = store.commit_code_intent(pending, NOW);
    panic!("crash point not reached");
}
#[test]
fn process_exit_after_commit_recovers_intent_and_effects_without_reply() {
    let f = Fixture::new();
    let (store, _) = formed(&f);
    drop(store);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "full_team_store::intent_tests::crash_child"])
        .env("FOUNDRY_INTENT_CRASH_ROOT", &f.root)
        .env(
            "FOUNDRY_INTENT_CRASH_PROFILE",
            serde_json::to_string(&f.profile).unwrap(),
        )
        .status()
        .unwrap();
    assert!(status.success());
    let mut store = reopen(&f);
    assert_eq!(store.recover_code_intents(NOW).unwrap().len(), 1);
    assert_eq!(store.code_effects(NOW).unwrap().len(), 2);
    assert_eq!(
        store.load(NOW).unwrap().document().unwrap().invites.len(),
        2
    );
}

#[test]
fn own_person_change_retires_old_code_without_resurrecting_it() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let pending = store.prepare_code_intent(&base, 1, true, NOW).unwrap();
    let issued = store.commit_code_intent(pending, NOW).unwrap();
    let lease = store.recover_code_intents(NOW).unwrap().pop().unwrap();
    let secret = &issued
        .loaded()
        .document()
        .unwrap()
        .link
        .as_ref()
        .unwrap()
        .body
        .secret;
    let pending = store
        .prepare_admission(
            issued.loaded(),
            crate::team_state::Admission {
                endpoint: f.identity.endpoint_id(),
                person: "new-person",
                machine: "desk",
                secret,
                proof: None,
                owner_only: false,
            },
            NOW + 1,
        )
        .unwrap();
    store.commit_transition(pending, NOW + 1).unwrap();
    assert!(matches!(
        store.validate_code_lease(&lease, NOW + 1),
        Err(StoreError::Conflict)
    ));
    assert!(store.recover_code_intents(NOW + 1).unwrap().is_empty());
    drop(store);
    assert!(reopen(&f).recover_code_intents(NOW + 1).unwrap().is_empty());
}
