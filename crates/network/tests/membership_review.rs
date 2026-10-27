mod common;
use foundry_network::{
    membership::{MembershipSnapshot, MembershipState},
    signing::{MemberRecord, MetadataRecord, SignedRecord},
};
use iroh::{EndpointId, SecretKey};
use std::collections::BTreeMap;

fn record(
    signer: &SecretKey,
    subject: EndpointId,
    name: &str,
    active: bool,
    ts: i64,
) -> SignedRecord<MemberRecord> {
    SignedRecord::sign(
        MemberRecord {
            endpoint: subject,
            person: name.into(),
            machine: "desk".into(),
            active,
            ts,
        },
        signer,
        "team-a",
        &common::config().profile,
    )
    .unwrap()
}
fn base(key: &SecretKey) -> MembershipSnapshot {
    let entry = record(key, key.public(), "founder", true, 1);
    MembershipSnapshot {
        team: "team-a".into(),
        founder: key.public(),
        metadata: SignedRecord::sign(
            MetadataRecord {
                name: "review".into(),
                relay: None,
                ts: 1,
            },
            key,
            "team-a",
            &common::config().profile,
        )
        .unwrap(),
        admissions: BTreeMap::from([(key.public().to_string(), entry.clone())]),
        members: BTreeMap::from([(key.public().to_string(), entry)]),
    }
}
fn boot(snapshot: &MembershipSnapshot) -> MembershipState {
    MembershipState::bootstrap(
        "team-a",
        snapshot.founder,
        snapshot,
        &common::config().profile,
        100,
    )
    .unwrap()
    .0
}
fn merge(
    state: &mut MembershipState,
    peer: &SecretKey,
    snapshot: &MembershipSnapshot,
) -> foundry_network::membership::MergeReport {
    state
        .merge_from(peer.public(), snapshot, &common::config().profile, 100)
        .unwrap()
}
fn add(snapshot: &mut MembershipSnapshot, entry: SignedRecord<MemberRecord>) {
    snapshot
        .admissions
        .insert(entry.body.endpoint.to_string(), entry.clone());
    snapshot
        .members
        .insert(entry.body.endpoint.to_string(), entry);
}

#[test]
fn disconnected_admission_cycle_never_acquires_signing_authority() {
    let founder = SecretKey::from_bytes(&[1; 32]);
    let a = SecretKey::from_bytes(&[2; 32]);
    let b = SecretKey::from_bytes(&[3; 32]);
    let original = base(&founder);
    let mut update = original.clone();
    let mut state = boot(&original);
    add(&mut update, record(&a, b.public(), "b", true, 2));
    add(&mut update, record(&b, a.public(), "a", true, 2));
    update.metadata = SignedRecord::sign(
        MetadataRecord {
            name: "cycle".into(),
            relay: None,
            ts: 2,
        },
        &a,
        "team-a",
        &common::config().profile,
    )
    .unwrap();
    let result = merge(&mut state, &founder, &update);
    assert_eq!(result.rejected, 5);
    assert!(!result.changed);
    assert_eq!(state.snapshot(), &original);
}

#[test]
fn foreign_team_signatures_cannot_be_transplanted_under_local_identity() {
    let founder = SecretKey::from_bytes(&[1; 32]);
    let peer = SecretKey::from_bytes(&[2; 32]);
    let original = base(&founder);
    let mut update = original.clone();
    let mut state = boot(&original);
    let body = record(&founder, peer.public(), "peer", true, 2).body;
    let foreign = SignedRecord::sign(body, &founder, "team-b", &common::config().profile).unwrap();
    add(&mut update, foreign);
    update.metadata = SignedRecord::sign(
        MetadataRecord {
            name: "other".into(),
            relay: None,
            ts: 2,
        },
        &founder,
        "team-b",
        &common::config().profile,
    )
    .unwrap();
    assert_eq!(merge(&mut state, &founder, &update).rejected, 3);
    assert_eq!(state.snapshot(), &original);
}

#[test]
fn revoked_member_replay_cannot_undo_durable_state_but_bootstrap_has_no_freshness() {
    let founder = SecretKey::from_bytes(&[1; 32]);
    let peer = SecretKey::from_bytes(&[2; 32]);
    let mut old = base(&founder);
    add(&mut old, record(&founder, peer.public(), "peer", true, 2));
    let mut state = boot(&old);
    let mut removed = old.clone();
    removed.members.insert(
        peer.public().to_string(),
        record(&founder, peer.public(), "peer", false, 3),
    );
    merge(&mut state, &founder, &removed);
    assert!(!state.is_active(peer.public()));
    assert!(!merge(&mut state, &founder, &old).changed);
    assert!(!state.is_active(peer.public()));
    assert!(
        boot(&old).is_active(peer.public()),
        "signatures alone cannot prove snapshot freshness"
    );
}

#[test]
fn conflicting_first_admissions_remain_order_dependent() {
    let founder = SecretKey::from_bytes(&[1; 32]);
    let peer = SecretKey::from_bytes(&[2; 32]);
    let original = base(&founder);
    let mut a = original.clone();
    let mut b = original.clone();
    add(&mut a, record(&founder, peer.public(), "peer", true, 2));
    add(&mut b, record(&founder, peer.public(), "peer", true, 3));
    let mut left = boot(&original);
    let mut right = boot(&original);
    merge(&mut left, &founder, &a);
    merge(&mut left, &founder, &b);
    merge(&mut right, &founder, &b);
    merge(&mut right, &founder, &a);
    assert_eq!(left.snapshot().members, right.snapshot().members);
    assert_ne!(left.snapshot().admissions, right.snapshot().admissions);
    let l = left.snapshot().clone();
    let r = right.snapshot().clone();
    assert!(!merge(&mut left, &founder, &r).changed);
    assert!(!merge(&mut right, &founder, &l).changed);
    assert_ne!(left.snapshot().admissions, right.snapshot().admissions);
}

#[test]
fn competing_name_claims_keep_the_first_accepted_owner() {
    let founder = SecretKey::from_bytes(&[1; 32]);
    let a = SecretKey::from_bytes(&[2; 32]);
    let b = SecretKey::from_bytes(&[3; 32]);
    let original = base(&founder);
    let mut first = original.clone();
    let mut second = original.clone();
    add(&mut first, record(&founder, a.public(), "shared", true, 2));
    add(&mut second, record(&founder, b.public(), "shared", true, 3));
    let mut left = boot(&original);
    let mut right = boot(&original);
    merge(&mut left, &founder, &first);
    assert_eq!(merge(&mut left, &founder, &second).proof_required, 1);
    merge(&mut right, &founder, &second);
    assert_eq!(merge(&mut right, &founder, &first).proof_required, 1);
    assert!(left.is_active(a.public()));
    assert!(!left.is_active(b.public()));
    assert!(right.is_active(b.public()));
    assert!(!right.is_active(a.public()));
    assert_ne!(left.snapshot().members, right.snapshot().members);
}

#[test]
fn invalid_high_timestamp_cannot_poison_later_valid_update_and_clock_boundary_is_inclusive() {
    let founder = SecretKey::from_bytes(&[1; 32]);
    let peer = SecretKey::from_bytes(&[2; 32]);
    let original = base(&founder);
    let mut state = boot(&original);
    let mut update = original.clone();
    let mut invalid = record(&founder, peer.public(), "peer", true, 86_400_100);
    invalid.sig = "ff".repeat(64);
    add(&mut update, invalid);
    assert_eq!(merge(&mut state, &founder, &update).rejected, 2);
    assert_eq!(state.snapshot(), &original);
    update = original.clone();
    add(
        &mut update,
        record(&founder, peer.public(), "peer", true, 86_400_100),
    );
    assert_eq!(merge(&mut state, &founder, &update).rejected, 0);
    assert!(state.is_active(peer.public()));
    let before = state.snapshot().clone();
    update.members.insert(
        peer.public().to_string(),
        record(&founder, peer.public(), "peer", false, 86_400_101),
    );
    assert_eq!(merge(&mut state, &founder, &update).rejected, 1);
    assert_eq!(state.snapshot(), &before);
}

#[test]
fn admission_without_live_member_can_sign_but_cannot_connect() {
    let founder = SecretKey::from_bytes(&[1; 32]);
    let peer = SecretKey::from_bytes(&[2; 32]);
    let original = base(&founder);
    let mut update = original.clone();
    let mut state = boot(&original);
    update.admissions.insert(
        peer.public().to_string(),
        record(&founder, peer.public(), "peer", true, 2),
    );
    update.metadata = SignedRecord::sign(
        MetadataRecord {
            name: "admitted".into(),
            relay: None,
            ts: 3,
        },
        &peer,
        "team-a",
        &common::config().profile,
    )
    .unwrap();
    assert_eq!(merge(&mut state, &founder, &update).rejected, 0);
    assert_eq!(state.snapshot().metadata.body.name, "admitted");
    assert!(!state.is_active(peer.public()));
    assert!(
        state
            .merge_from(peer.public(), &update, &common::config().profile, 100)
            .is_err()
    );
}
