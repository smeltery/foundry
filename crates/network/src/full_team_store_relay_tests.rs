use super::*;
use std::{fs, os::unix::fs::DirBuilderExt, path::PathBuf};

const NOW: i64 = 100;
const WAIT: Duration = Duration::from_secs(3);
struct Fixture {
    root: PathBuf,
    identity: Arc<LocalIdentity>,
    profile: WireProfile,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("relay-{}", iroh::SecretKey::generate().public()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let dir = cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
        let identity = Arc::new(crate::load_or_create_identity(&dir, WAIT).unwrap());
        let profile = WireProfile {
            team_alpn: b"fixture/team".to_vec(),
            join_alpn: b"fixture/join".to_vec(),
            protocol: 1,
            signing_prefix: "fixture/sign/".into(),
            invitation_endpoint_context: "fixture endpoint".into(),
            invitation_secret_context: "fixture secret".into(),
            ticket_prefixes: vec!["fixture-".into()],
        };
        Self {
            root,
            identity,
            profile,
        }
    }
    fn store(&self, create: bool) -> FullTeamStore {
        let policy = StorePolicy::new(self.profile.clone(), None).unwrap();
        if create {
            FullTeamStore::create_new(&self.root, self.identity.clone(), policy, WAIT).unwrap()
        } else {
            FullTeamStore::open_existing(&self.root, self.identity.clone(), policy, WAIT).unwrap()
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn formation() -> Formation<'static> {
    Formation {
        name: "group",
        person: "alice",
        machine: "desk",
    }
}

#[test]
fn formation_and_reopen_accept_current_epoch_milliseconds() {
    let f = Fixture::new();
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    TeamState::found(
        formation(),
        f.identity.secret_key(),
        &f.profile,
        now,
        &mut SystemEntropy,
    )
    .unwrap();
    let mut store = f.store(true);
    let empty = store.load(now).unwrap();
    let pending = store.prepare_formation(&empty, formation(), now).unwrap();
    let formed = store.commit_formation(pending).unwrap();
    assert_eq!(formed.revision(), 1);
    drop(store);
    assert_eq!(f.store(false).load(now).unwrap().revision(), 1);
}

#[test]
fn cancelled_first_write_preserves_empty_checkpoint_and_allows_retry() {
    let f = Fixture::new();
    let mut store = f.store(true);
    let empty = store.load(NOW).unwrap();
    let pending = store.prepare_formation(&empty, formation(), NOW).unwrap();
    assert!(matches!(
        store.commit_formation_when(pending, || false),
        Err(StoreError::Cancelled)
    ));
    let unchanged = store.load(NOW).unwrap();
    assert_eq!(unchanged.revision(), 0);
    assert!(unchanged.document().is_none());
    drop(store);
    let mut reopened = f.store(false);
    let empty = reopened.load(NOW).unwrap();
    let retry = reopened
        .prepare_formation(&empty, formation(), NOW)
        .unwrap();
    assert_eq!(
        reopened
            .commit_formation_when(retry, || true)
            .unwrap()
            .revision(),
        1
    );
}
fn active(outcome: CommitOutcome) -> LoadedTeam {
    match outcome {
        CommitOutcome::Active(team) => team,
        _ => panic!("expected committed active team"),
    }
}

#[test]
fn initial_relay_is_atomic_signed_and_reopens_exactly() {
    let f = Fixture::new();
    let mut store = f.store(true);
    let empty = store.load(NOW).unwrap();
    let pending = store
        .prepare_formation_with_relay(&empty, formation(), Some("https://relay.invalid"), NOW)
        .unwrap();
    assert!(store.load(NOW).unwrap().document().is_none());
    let formed = store.commit_formation(pending).unwrap();
    assert_eq!(formed.revision(), 1);
    let document = formed.document().unwrap();
    assert_eq!(document.meta.body.name, "group");
    assert_eq!(
        document.meta.body.relay.as_deref(),
        Some("https://relay.invalid")
    );
    assert_eq!(document.meta.by, f.identity.endpoint_id());
    let encoded = serde_json::to_vec(document).unwrap();
    drop(store);
    assert_eq!(
        encoded,
        serde_json::to_vec(f.store(false).load(NOW).unwrap().document().unwrap()).unwrap()
    );
}

#[test]
fn relay_updates_preserve_other_sections_and_reject_stale_writers() {
    let f = Fixture::new();
    let mut first = f.store(true);
    let empty = first.load(NOW).unwrap();
    let formed = first
        .commit_formation(first.prepare_formation(&empty, formation(), NOW).unwrap())
        .unwrap();
    let mut other = f.store(false);
    let other_base = other.load(NOW).unwrap();
    let stale = other
        .prepare_relay(&other_base, Some("https://stale.invalid"), NOW)
        .unwrap();
    let pending = first
        .prepare_relay(&formed, Some("https://relay.invalid"), NOW)
        .unwrap();
    let changed = active(first.commit_transition(pending, NOW).unwrap());
    let before = formed.document().unwrap();
    let after = changed.document().unwrap();
    assert!(after.meta.body.ts > before.meta.body.ts);
    let mut expected = serde_json::to_value(before).unwrap();
    expected["meta"] = serde_json::to_value(&after.meta).unwrap();
    assert_eq!(expected, serde_json::to_value(after).unwrap());
    assert!(matches!(
        other.commit_transition(stale, NOW),
        Err(StoreError::Conflict)
    ));
    let pending = first.prepare_relay(&changed, None, NOW).unwrap();
    let cleared = active(first.commit_transition(pending, NOW).unwrap());
    assert!(cleared.document().unwrap().meta.body.relay.is_none());
    assert!(cleared.document().unwrap().meta.body.ts > after.meta.body.ts);
    let expected = serde_json::to_vec(cleared.document().unwrap()).unwrap();
    drop(first);
    assert_eq!(
        expected,
        serde_json::to_vec(other.load(NOW).unwrap().document().unwrap()).unwrap()
    );
}

#[test]
fn oversized_relay_and_foreign_base_cannot_change_empty_store() {
    let f = Fixture::new();
    let mut store = f.store(true);
    let empty = store.load(NOW).unwrap();
    let oversized = "x".repeat(MAX_CHECKPOINT_BYTES + 1);
    assert!(matches!(
        store.prepare_formation_with_relay(&empty, formation(), Some(&oversized), NOW),
        Err(StoreError::TooLarge)
    ));
    let mut other = f.store(false);
    let foreign = other.load(NOW).unwrap();
    assert!(matches!(
        store.prepare_formation_with_relay(&foreign, formation(), None, NOW),
        Err(StoreError::Binding)
    ));
    assert_eq!(store.load(NOW).unwrap().revision(), 0);
    assert!(store.load(NOW).unwrap().document().is_none());
}

#[test]
fn relay_signer_must_be_active_but_need_not_be_founder() {
    use crate::team_state::{Admission, TeamError};
    let f = Fixture::new();
    let owner = f.identity.secret_key();
    let member = iroh::SecretKey::generate();
    let outsider = iroh::SecretKey::generate();
    let state = TeamState::found(formation(), owner, &f.profile, NOW, &mut SystemEntropy).unwrap();
    assert!(matches!(
        state.set_relay(&outsider, None, NOW),
        Err(TeamError::Unauthorized)
    ));
    let state = state
        .admit(
            owner,
            Admission {
                endpoint: member.public(),
                person: "bob",
                machine: "laptop",
                secret: &state.document().link.as_ref().unwrap().body.secret,
                proof: None,
                owner_only: false,
            },
            NOW,
        )
        .unwrap();
    let updated = state
        .set_relay(&member, Some("https://relay.invalid"), NOW)
        .unwrap();
    assert_eq!(updated.document().meta.by, member.public());
    updated
        .document()
        .meta
        .verify(&updated.document().id, &f.profile)
        .unwrap();
    let same = updated
        .set_relay(&member, Some("https://relay.invalid"), NOW)
        .unwrap();
    assert!(same.document().meta.body.ts > updated.document().meta.body.ts);
    let removed = same
        .remove(owner, &[member.public()], NOW, &mut SystemEntropy)
        .unwrap();
    assert!(matches!(
        removed.set_relay(&member, None, NOW),
        Err(TeamError::Unauthorized)
    ));
}
