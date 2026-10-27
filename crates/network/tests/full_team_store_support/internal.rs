use super::*;
use crate::{signing::SignedRecord, team_state::Admission};
use std::{fs, os::unix::fs::DirBuilderExt, path::PathBuf};
const NOW: i64 = 1_000_000;
pub(super) struct Fixture {
    pub(super) root: PathBuf,
    pub(super) identity: Arc<LocalIdentity>,
    pub(super) profile: WireProfile,
}
impl Fixture {
    pub(super) fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "full-store-{}",
            iroh::SecretKey::generate().public()
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let dir = cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
        let identity =
            Arc::new(crate::load_or_create_identity(&dir, Duration::from_secs(1)).unwrap());
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
    pub(super) fn policy(&self) -> StorePolicy {
        StorePolicy::new(self.profile.clone(), None).unwrap()
    }
    pub(super) fn store(&self) -> FullTeamStore {
        FullTeamStore::create_new(
            &self.root,
            self.identity.clone(),
            self.policy(),
            Duration::from_secs(1),
        )
        .unwrap()
    }
    pub(super) fn formation(&self) -> TeamState {
        TeamState::found(
            Formation {
                name: "group",
                person: "alice",
                machine: "desk",
            },
            self.identity.secret_key(),
            &self.profile,
            NOW,
            &mut SystemEntropy,
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
pub(super) fn add(
    state: &TeamState,
    actor: &iroh::SecretKey,
    peer: &iroh::SecretKey,
    person: &str,
    secret: &str,
) -> TeamState {
    state
        .admit(
            actor,
            Admission {
                endpoint: peer.public(),
                person,
                machine: "desk",
                secret,
                proof: None,
                owner_only: false,
            },
            NOW,
        )
        .unwrap()
}
// Test-only sealing permits adversarial signed-invalid documents and historical
// fixtures. There is deliberately no corresponding public production constructor.
pub(super) fn seal(store: &FullTeamStore, document: TeamDocument) {
    let (base, digest) =
        checkpoint::read(&store.connection, &store.identity, &store.policy).unwrap();
    let mut next = Checkpoint::empty(
        base.incarnation,
        store.identity.endpoint_id(),
        store.policy.digest,
    );
    next.revision = 1;
    next.parent = digest;
    next.accepted_ms = NOW + 1;
    next.state = StoredState::Active {
        document: Box::new(document),
    };
    checkpoint::write(&store.connection, &next, &store.identity, false).unwrap();
}
#[test]
fn cold_reload_preserves_retained_siblings_without_empty_bootstrap() {
    let f = Fixture::new();
    let store = f.store();
    let bob = iroh::SecretKey::from_bytes(&[2; 32]);
    let first = iroh::SecretKey::from_bytes(&[3; 32]);
    let second = iroh::SecretKey::from_bytes(&[4; 32]);
    let mut state = f.formation();
    state = add(
        &state,
        f.identity.secret_key(),
        &bob,
        "bob",
        &state.document().link.as_ref().unwrap().body.secret,
    );
    for (key, secret) in [(&first, "one"), (&second, "two")] {
        state = state.issue_invite(&bob, secret, 1, true, NOW).unwrap();
        state = add(&state, &bob, key, "bob", secret);
    }
    state = state.departure(&bob, NOW + 1).unwrap();
    let (filtered, report) = TeamState::bootstrap(
        &state.document().id,
        f.identity.endpoint_id(),
        state.document(),
        &f.profile,
        NOW + 1,
    )
    .unwrap();
    assert!(report.rejected > 0 && filtered.document() != state.document());
    seal(&store, state.document().clone());
    drop(store);
    let mut reopened = FullTeamStore::open_existing(
        &f.root,
        f.identity.clone(),
        f.policy(),
        Duration::from_secs(1),
    )
    .unwrap();
    let loaded = reopened.load(NOW).unwrap();
    assert!(loaded.document().unwrap() == state.document());
    for key in [&first, &second] {
        assert!(
            loaded.document().unwrap().members[&key.public().to_string()]
                .body
                .active
        );
    }
}
#[test]
fn valid_local_seal_never_hides_invalid_records_or_map_identity() {
    let f = Fixture::new();
    let mut store = f.store();
    let original = f.formation().document().clone();
    for kind in 0..7 {
        let mut bad = original.clone();
        match kind {
            0 => bad.meta.sig = "invalid".into(),
            1 => bad.link.as_mut().unwrap().sig = "invalid".into(),
            2 => bad.invites.values_mut().next().unwrap().sig = "invalid".into(),
            3 => bad.admissions.values_mut().next().unwrap().sig = "invalid".into(),
            4 => bad.members.values_mut().next().unwrap().sig = "invalid".into(),
            5 => {
                let record = bad.members.values().next().unwrap().clone();
                bad.members.insert("wrong-key".into(), record);
            }
            _ => bad.id = "wrong-team".into(),
        }
        seal(&store, bad);
        assert!(matches!(store.load(NOW), Err(StoreError::Corrupt)));
    }
    let mut bad = original;
    let mut body = bad.meta.body.clone();
    body.ts = NOW + 86_400_002;
    bad.meta = SignedRecord::sign(body, f.identity.secret_key(), &bad.id, &f.profile).unwrap();
    seal(&store, bad);
    assert!(matches!(store.load(NOW), Err(StoreError::Corrupt)));
}
#[test]
fn proof_policy_is_bound_and_ownership_is_reverified_on_reload() {
    let f = Fixture::new();
    let vectors: serde_json::Value =
        serde_json::from_str(include_str!("../team_state_support/proofs.json")).unwrap();
    let bytes = serde_json::to_vec(&vectors["jwks"]).unwrap();
    let policy = || {
        StorePolicy::new(
            f.profile.clone(),
            Some((
                ProofConfig {
                    issuer: "team-issuer".into(),
                    audience: "team-audience".into(),
                    token_type: "team-proof".into(),
                },
                &bytes,
            )),
        )
        .unwrap()
    };
    let mut store = FullTeamStore::create_new(
        &f.root,
        f.identity.clone(),
        policy(),
        Duration::from_secs(1),
    )
    .unwrap();
    let peer = iroh::SecretKey::from_bytes(&[2; 32]);
    let state = f.formation();
    let state = add(
        &state,
        f.identity.secret_key(),
        &peer,
        "alex",
        &state.document().link.as_ref().unwrap().body.secret,
    )
    .with_proof_verifier(store.policy.verifier.clone().unwrap())
    .unwrap();
    let state = state
        .attach_proof(
            peer.public(),
            vectors["tokens"]["expired_owner"].as_str().unwrap(),
            NOW,
        )
        .unwrap();
    seal(&store, state.document().clone());
    assert!(store.load(NOW).unwrap().document().unwrap() == state.document());
    let mut bad = state.document().clone();
    bad.attestations
        .insert(peer.public().to_string(), "invalid".into());
    seal(&store, bad);
    assert!(matches!(store.load(NOW), Err(StoreError::Corrupt)));
    drop(store);
    assert!(matches!(
        FullTeamStore::open_existing(
            &f.root,
            f.identity.clone(),
            f.policy(),
            Duration::from_secs(1)
        ),
        Err(StoreError::Binding)
    ));
}
#[test]
fn nonempty_intents_and_invalid_ancestry_are_not_silently_filtered() {
    let f = Fixture::new();
    let mut store = f.store();
    let (base, _) = checkpoint::read(&store.connection, &store.identity, &store.policy).unwrap();
    let mut data = serde_json::to_value(&base).unwrap();
    data["intents"] = serde_json::json!([null]);
    let altered: Checkpoint = serde_json::from_value(data).unwrap();
    checkpoint::write(&store.connection, &altered, &store.identity, false).unwrap();
    assert!(matches!(store.load(NOW), Err(StoreError::Unsupported)));
    checkpoint::write(&store.connection, &base, &store.identity, false).unwrap();
    let state = f.formation();
    let mut doc = state.document().clone();
    let unknown = iroh::SecretKey::from_bytes(&[8; 32]);
    let mut body = doc.admissions.values().next().unwrap().body.clone();
    body.endpoint = unknown.public();
    doc.admissions.insert(
        unknown.public().to_string(),
        SignedRecord::sign(body, &unknown, &doc.id, &f.profile).unwrap(),
    );
    seal(&store, doc);
    assert!(matches!(store.load(NOW), Err(StoreError::Corrupt)));
    let mut departed = state.document().clone();
    let entry = departed
        .members
        .get_mut(&f.identity.endpoint_id().to_string())
        .unwrap();
    let mut body = entry.body.clone();
    body.active = false;
    *entry = SignedRecord::sign(body, f.identity.secret_key(), &departed.id, &f.profile).unwrap();
    seal(&store, departed);
    assert!(matches!(store.load(NOW), Err(StoreError::Unsupported)));
}
#[test]
fn locally_accepted_retry_label_conflict_survives_cold_reload() {
    let f = Fixture::new();
    let mut store = f.store();
    let bob = iroh::SecretKey::from_bytes(&[2; 32]);
    let carol = iroh::SecretKey::from_bytes(&[3; 32]);
    let state = f.formation();
    let state = add(
        &state,
        f.identity.secret_key(),
        &carol,
        "carol",
        &state.document().link.as_ref().unwrap().body.secret,
    );
    let state = state
        .issue_invite(f.identity.secret_key(), "one-use", 1, false, NOW)
        .unwrap();
    let state = add(&state, f.identity.secret_key(), &bob, "bob", "one-use");
    let state = add(&state, f.identity.secret_key(), &bob, "carol", "one-use");
    seal(&store, state.document().clone());
    let loaded = store.load(NOW).unwrap();
    assert!(loaded.document().unwrap() == state.document());
    assert_eq!(
        loaded
            .document()
            .unwrap()
            .members
            .values()
            .filter(|m| m.body.person == "carol")
            .count(),
        2
    );
}
