use super::tests::{Fixture, add, seal};
use super::*;
use iroh::SecretKey;
const NOW: i64 = 1_000_000;
fn formed(f: &Fixture) -> (FullTeamStore, LoadedTeam) {
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
    let loaded = store.commit_formation(pending).unwrap();
    (store, loaded)
}
fn assert_absent(root: &Path, needle: &[u8]) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = std::fs::read(path).unwrap();
            assert!(!bytes.windows(needle.len()).any(|w| w == needle));
        }
    }
}
#[test]
fn validated_removal_never_encodes_incoming_secrets_and_scrubs_old_secret() {
    let f = Fixture::new();
    let mut store = f.store();
    let peer = SecretKey::from_bytes(&[2; 32]);
    let state = f.formation();
    let state = add(
        &state,
        f.identity.secret_key(),
        &peer,
        "bob",
        &state.document().link.as_ref().unwrap().body.secret,
    );
    seal(&store, state.document().clone());
    let old_secret = state.document().link.as_ref().unwrap().body.secret.clone();
    let base = store.load(NOW).unwrap();
    let removed = state
        .remove(
            &peer,
            &[f.identity.endpoint_id()],
            NOW + 1,
            &mut SystemEntropy,
        )
        .unwrap();
    let mut incoming = removed.document().clone();
    incoming.link.as_mut().unwrap().body.secret = "replacement-link-sentinel".into();
    let mut invite = incoming.invites.values().next().unwrap().clone();
    invite.body.hash = "replacement-invite-sentinel".into();
    incoming.invites.insert(invite.body.hash.clone(), invite);
    let _probe = checkpoint::forbid_encoding(vec![
        b"replacement-link-sentinel".to_vec(),
        b"replacement-invite-sentinel".to_vec(),
    ]);
    let mut oversized = incoming.clone();
    oversized
        .link
        .as_mut()
        .unwrap()
        .body
        .secret
        .push_str(&"x".repeat(MAX_CHECKPOINT_BYTES));
    assert!(matches!(
        store.prepare_merge(&base, peer.public(), &oversized, NOW + 1),
        Err(StoreError::TooLarge)
    ));
    assert!(store.load(NOW).unwrap().document() == base.document());
    let (pending, _) = store
        .prepare_merge(&base, peer.public(), &incoming, NOW + 1)
        .unwrap();
    assert!(pending.requires_quiescence());
    match store.commit_transition(pending, NOW + 1).unwrap() {
        CommitOutcome::Removed { scrub, .. } => assert_eq!(scrub, ScrubStatus::Complete),
        _ => panic!("expected removal"),
    }
    assert_absent(&f.root, b"replacement-link-sentinel");
    assert_absent(&f.root, b"replacement-invite-sentinel");
    assert_absent(&f.root, old_secret.as_bytes());
}
#[test]
fn forged_stale_losing_and_unauthorized_removals_do_not_signal_quiescence() {
    let f = Fixture::new();
    let mut store = f.store();
    let peer = SecretKey::from_bytes(&[2; 32]);
    let state = f.formation();
    let state = add(
        &state,
        f.identity.secret_key(),
        &peer,
        "bob",
        &state.document().link.as_ref().unwrap().body.secret,
    );
    seal(&store, state.document().clone());
    let base = store.load(NOW).unwrap();
    let local = f.identity.endpoint_id().to_string();
    let mut incoming = state.document().clone();
    let mut body = incoming.members[&local].body.clone();
    body.active = false;
    for kind in 0..3 {
        let mut body = body.clone();
        body.ts = if kind == 0 {
            NOW - 1
        } else if kind == 1 {
            NOW
        } else {
            NOW + 1
        };
        let mut record =
            crate::signing::SignedRecord::sign(body, &peer, &incoming.id, &f.profile).unwrap();
        if kind == 1 {
            // deterministically lose timestamp/signature ordering.
            record.body.ts = NOW - 1;
            record =
                crate::signing::SignedRecord::sign(record.body, &peer, &incoming.id, &f.profile)
                    .unwrap();
        }
        if kind == 2 {
            record.sig = "forged".into();
        }
        incoming.members.insert(local.clone(), record);
        let (pending, report) = store
            .prepare_merge(&base, peer.public(), &incoming, NOW + 1)
            .unwrap();
        assert!(!pending.requires_quiescence());
        if kind == 2 {
            assert!(report.rejected > 0);
        }
    }
    assert!(
        store
            .prepare_merge(
                &base,
                SecretKey::from_bytes(&[9; 32]).public(),
                &incoming,
                NOW + 1
            )
            .is_err()
    );
}
#[test]
fn indeterminate_sync_preserves_latched_removal_and_reload_recovers() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let pending = store.prepare_departure(&base, NOW + 1).unwrap();
    let quiesce = pending.requires_quiescence();
    store.fail_sync = true;
    assert!(matches!(
        store.commit_transition(pending, NOW + 1),
        Err(StoreError::Indeterminate)
    ));
    assert!(quiesce);
    store.fail_sync = false;
    let receipt = match store.load_status(NOW + 1).unwrap() {
        LoadedStatus::Removed(receipt) => receipt,
        _ => panic!("expected tombstone"),
    };
    assert_eq!(store.retry_scrub(&receipt).unwrap(), ScrubStatus::Complete);
}
#[test]
fn revision_exhaustion_and_clock_horizon_do_not_weaken_operation_checks() {
    let f = Fixture::new();
    let (mut store, _) = formed(&f);
    let (mut checkpoint, _) =
        checkpoint::read(&store.connection, &store.identity, &store.policy).unwrap();
    checkpoint.revision = i64::MAX as u64;
    checkpoint::write(&store.connection, &checkpoint, &store.identity, false).unwrap();
    let base = store.load(NOW).unwrap();
    assert!(matches!(
        store.prepare_rotation(&base, NOW + 1),
        Err(StoreError::RevisionOverflow)
    ));
    checkpoint.revision = 1;
    checkpoint.accepted_ms = NOW + 100_000_000;
    checkpoint::write(&store.connection, &checkpoint, &store.identity, false).unwrap();
    let base = store.load(NOW).unwrap();
    let mut incoming = base.document().unwrap().clone();
    let mut meta = incoming.meta.body.clone();
    meta.ts = NOW + 86_400_001;
    incoming.meta =
        crate::signing::SignedRecord::sign(meta, f.identity.secret_key(), &incoming.id, &f.profile)
            .unwrap();
    let (pending, report) = store
        .prepare_merge(&base, f.identity.endpoint_id(), &incoming, NOW)
        .unwrap();
    assert!(report.rejected > 0);
    match store.commit_transition(pending, NOW).unwrap() {
        CommitOutcome::Unchanged(state) => assert_eq!(state.revision(), 1),
        _ => panic!("future record accepted"),
    }
}
#[test]
fn scrub_sync_failure_is_confirmed_removed_but_not_complete() {
    let f = Fixture::new();
    let (mut store, base) = formed(&f);
    let pending = store.prepare_departure(&base, NOW + 1).unwrap();
    store.fail_scrub_sync = true;
    let receipt = match store.commit_transition(pending, NOW + 1).unwrap() {
        CommitOutcome::Removed { receipt, scrub } => {
            assert_eq!(scrub, ScrubStatus::Failed(ScrubFailure::Sync));
            receipt
        }
        _ => panic!("expected removal"),
    };
    store.fail_scrub_sync = false;
    assert_eq!(store.retry_scrub(&receipt).unwrap(), ScrubStatus::Complete);
}
#[test]
#[ignore = "subprocess entry point"]
fn crash_after_commit_child() {
    let root = std::path::PathBuf::from(std::env::var_os("FOUNDRY_TEST_TEAM_ROOT").unwrap());
    let profile: WireProfile =
        serde_json::from_str(&std::env::var("FOUNDRY_TEST_TEAM_PROFILE").unwrap()).unwrap();
    let dir = cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
    let identity = Arc::new(crate::load_or_create_identity(&dir, Duration::from_secs(1)).unwrap());
    let mut store = FullTeamStore::open_existing(
        &root,
        identity,
        StorePolicy::new(profile, None).unwrap(),
        Duration::from_secs(1),
    )
    .unwrap();
    let base = store.load(NOW).unwrap();
    let pending = store.prepare_departure(&base, NOW + 1).unwrap();
    store.crash_after_commit = true;
    let _ = store.commit_transition(pending, NOW + 1);
    panic!("crash boundary not reached");
}
#[test]
fn process_exit_after_commit_before_scrub_recovers_removed_state() {
    use std::process::{Command, Stdio};
    let f = Fixture::new();
    let (store, base) = formed(&f);
    let secret = base
        .document()
        .unwrap()
        .link
        .as_ref()
        .unwrap()
        .body
        .secret
        .clone();
    drop(store);
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Child(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "full_team_store::transition_tests::crash_after_commit_child",
            ])
            .env("FOUNDRY_TEST_TEAM_ROOT", &f.root)
            .env(
                "FOUNDRY_TEST_TEAM_PROFILE",
                serde_json::to_string(&f.profile).unwrap(),
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(std::time::Instant::now() < deadline, "child timeout");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(status.success());
    drop(child);
    let mut reopened = FullTeamStore::open_existing(
        &f.root,
        f.identity.clone(),
        f.policy(),
        Duration::from_secs(1),
    )
    .unwrap();
    let receipt = match reopened.load_status(NOW + 1).unwrap() {
        LoadedStatus::Removed(receipt) => receipt,
        _ => panic!("removal lost"),
    };
    assert_eq!(
        reopened.retry_scrub(&receipt).unwrap(),
        ScrubStatus::Complete
    );
    assert_absent(&f.root, secret.as_bytes());
}
