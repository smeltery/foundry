mod common;
use foundry_network::{
    WireProfile,
    membership::{MembershipSnapshot, MembershipState},
    signing::{MemberRecord, MetadataRecord, SignedRecord},
};
use iroh::{EndpointId, SecretKey};
use std::collections::BTreeMap;

const TEAM: &str = "team-a";
fn profile() -> WireProfile {
    common::config().profile
}
fn member(
    signer: &SecretKey,
    endpoint: EndpointId,
    person: &str,
    active: bool,
    ts: i64,
) -> SignedRecord<MemberRecord> {
    SignedRecord::sign(
        MemberRecord {
            endpoint,
            person: person.into(),
            machine: "desk".into(),
            active,
            ts,
        },
        signer,
        TEAM,
        &profile(),
    )
    .unwrap()
}
fn initial(founder: &SecretKey) -> MembershipSnapshot {
    let entry = member(founder, founder.public(), "founder", true, 1);
    MembershipSnapshot {
        team: TEAM.into(),
        founder: founder.public(),
        metadata: SignedRecord::sign(
            MetadataRecord {
                name: "sample".into(),
                relay: None,
                ts: 1,
            },
            founder,
            TEAM,
            &profile(),
        )
        .unwrap(),
        admissions: BTreeMap::from([(founder.public().to_string(), entry.clone())]),
        members: BTreeMap::from([(founder.public().to_string(), entry)]),
    }
}
fn boot(snapshot: &MembershipSnapshot) -> MembershipState {
    MembershipState::bootstrap(TEAM, snapshot.founder, snapshot, &profile(), 100)
        .unwrap()
        .0
}
fn add(snapshot: &mut MembershipSnapshot, record: SignedRecord<MemberRecord>) {
    let id = record.body.endpoint.to_string();
    snapshot.admissions.insert(id.clone(), record.clone());
    snapshot.members.insert(id, record);
}
fn merge(
    state: &mut MembershipState,
    peer: &SecretKey,
    update: &MembershipSnapshot,
) -> foundry_network::membership::MergeReport {
    state
        .merge_from(peer.public(), update, &profile(), 100)
        .unwrap()
}

#[test]
fn chained_admissions_reach_fixed_point_and_nonfounder_may_update_metadata() {
    let founder = SecretKey::generate();
    let second = SecretKey::generate();
    let third = SecretKey::generate();
    let mut update = initial(&founder);
    let mut state = boot(&update);
    add(
        &mut update,
        member(&founder, second.public(), "second", true, 2),
    );
    add(
        &mut update,
        member(&second, third.public(), "third", true, 3),
    );
    update.metadata = SignedRecord::sign(
        MetadataRecord {
            name: "changed".into(),
            relay: None,
            ts: 4,
        },
        &third,
        TEAM,
        &profile(),
    )
    .unwrap();
    let report = merge(&mut state, &founder, &update);
    assert!(report.changed);
    assert_eq!(report.rejected, 0);
    assert!(state.is_active(second.public()) && state.is_active(third.public()));
    assert_eq!(state.snapshot().metadata.body.name, "changed");
    assert!(!merge(&mut state, &second, &update).changed);
}

#[test]
fn unknown_peer_cannot_bootstrap_itself_through_an_update() {
    let founder = SecretKey::generate();
    let stranger = SecretKey::generate();
    let mut update = initial(&founder);
    let mut state = boot(&update);
    let before = state.snapshot().clone();
    add(
        &mut update,
        member(&founder, stranger.public(), "new", true, 2),
    );
    assert!(
        state
            .merge_from(stranger.public(), &update, &profile(), 100)
            .is_err()
    );
    assert_eq!(*state.snapshot(), before);
    update.team = "team-b".into();
    assert!(
        state
            .merge_from(founder.public(), &update, &profile(), 100)
            .is_err()
    );
    update.team = TEAM.into();
    update.founder = stranger.public();
    assert!(
        state
            .merge_from(founder.public(), &update, &profile(), 100)
            .is_err()
    );
}

#[test]
fn invalid_records_are_reported_without_mutating_trusted_state() {
    let founder = SecretKey::generate();
    let stranger = SecretKey::generate();
    let mut update = initial(&founder);
    let mut state = boot(&update);
    let before = state.snapshot().clone();
    add(
        &mut update,
        member(&stranger, stranger.public(), "new", true, 2),
    );
    update.metadata = SignedRecord::sign(
        MetadataRecord {
            name: "forged".into(),
            relay: None,
            ts: 5,
        },
        &stranger,
        TEAM,
        &profile(),
    )
    .unwrap();
    let report = merge(&mut state, &founder, &update);
    assert!(!report.changed);
    assert_eq!(report.rejected, 3);
    assert_eq!(*state.snapshot(), before);
    let mut forged = member(&founder, stranger.public(), "new", true, 2);
    forged.body.machine.push('x');
    update = before.clone();
    add(&mut update, forged);
    assert_eq!(merge(&mut state, &founder, &update).rejected, 2);
    assert_eq!(*state.snapshot(), before);
}

#[test]
fn removal_blocks_peer_and_self_return_but_keeps_historical_signing_authority() {
    let founder = SecretKey::generate();
    let second = SecretKey::generate();
    let mut update = initial(&founder);
    add(
        &mut update,
        member(&founder, second.public(), "second", true, 2),
    );
    let mut state = boot(&update);
    update.members.insert(
        second.public().to_string(),
        member(&second, second.public(), "second", false, 3),
    );
    merge(&mut state, &second, &update);
    assert!(!state.is_active(second.public()));
    assert!(
        state
            .merge_from(second.public(), &update, &profile(), 100)
            .is_err()
    );
    update.members.insert(
        second.public().to_string(),
        member(&second, second.public(), "second", true, 4),
    );
    update.metadata = SignedRecord::sign(
        MetadataRecord {
            name: "historical".into(),
            relay: None,
            ts: 4,
        },
        &second,
        TEAM,
        &profile(),
    )
    .unwrap();
    let report = merge(&mut state, &founder, &update);
    assert_eq!(report.rejected, 1);
    assert!(!state.is_active(second.public()));
    assert_eq!(state.snapshot().metadata.body.name, "historical");
    update.members.insert(
        second.public().to_string(),
        member(&founder, second.public(), "second", true, 5),
    );
    merge(&mut state, &founder, &update);
    assert!(state.is_active(second.public()));
    assert_eq!(
        state.snapshot().admissions[&second.public().to_string()]
            .body
            .ts,
        2
    );
}

#[test]
fn compatible_lww_updates_converge_with_signature_tiebreak() {
    let founder = SecretKey::generate();
    let second = SecretKey::generate();
    let mut base = initial(&founder);
    add(
        &mut base,
        member(&founder, second.public(), "second", true, 2),
    );
    let mut a = base.clone();
    let mut b = base.clone();
    a.metadata = SignedRecord::sign(
        MetadataRecord {
            name: "alpha".into(),
            relay: None,
            ts: 10,
        },
        &founder,
        TEAM,
        &profile(),
    )
    .unwrap();
    b.metadata = SignedRecord::sign(
        MetadataRecord {
            name: "beta".into(),
            relay: None,
            ts: 10,
        },
        &second,
        TEAM,
        &profile(),
    )
    .unwrap();
    a.members.insert(
        second.public().to_string(),
        member(&founder, second.public(), "second", false, 11),
    );
    b.members.insert(
        second.public().to_string(),
        member(&founder, second.public(), "second", true, 12),
    );
    let mut left = boot(&base);
    let mut right = boot(&base);
    merge(&mut left, &founder, &a);
    merge(&mut left, &founder, &b);
    merge(&mut right, &founder, &b);
    merge(&mut right, &founder, &a);
    assert_eq!(left.snapshot(), right.snapshot());
    assert!(left.is_active(second.public()));
    assert_eq!(
        left.snapshot().metadata.sig,
        a.metadata.sig.max(b.metadata.sig)
    );
}

#[test]
fn same_person_member_can_vouch_but_other_name_claim_needs_proof_adapter() {
    let founder = SecretKey::generate();
    let second = SecretKey::generate();
    let third = SecretKey::generate();
    let mut base = initial(&founder);
    add(
        &mut base,
        member(&founder, second.public(), "second", true, 2),
    );
    let mut state = boot(&base);
    add(
        &mut base,
        member(&founder, third.public(), "second", true, 3),
    );
    let report = merge(&mut state, &founder, &base);
    assert_eq!(report.proof_required, 1);
    assert!(!state.is_active(third.public()));
    base.members.insert(
        third.public().to_string(),
        member(&second, third.public(), "second", true, 4),
    );
    let report = merge(&mut state, &second, &base);
    assert_eq!(report.proof_required, 0);
    assert!(state.is_active(third.public()));
}

#[test]
fn bad_foundation_domain_future_clock_and_projection_fields_are_explicit() {
    let founder = SecretKey::generate();
    let stranger = SecretKey::generate();
    let base = initial(&founder);
    assert!(MembershipState::bootstrap("other", founder.public(), &base, &profile(), 100).is_err());
    let mut bad = base.clone();
    bad.admissions.clear();
    assert!(MembershipState::bootstrap(TEAM, founder.public(), &bad, &profile(), 100).is_err());
    bad = base.clone();
    bad.metadata.sig = "00".repeat(64);
    assert!(MembershipState::bootstrap(TEAM, founder.public(), &bad, &profile(), 100).is_err());
    let mut state = boot(&base);
    let mut changed = profile();
    changed.signing_prefix.push('x');
    assert!(
        state
            .merge_from(founder.public(), &base, &changed, 100)
            .is_err()
    );
    bad = base.clone();
    add(
        &mut bad,
        member(&founder, stranger.public(), "new", true, 86_400_101),
    );
    assert_eq!(merge(&mut state, &founder, &bad).rejected, 2);
    let mut value = serde_json::to_value(&base).unwrap();
    value["invites"] = serde_json::json!({});
    assert!(serde_json::from_value::<MembershipSnapshot>(value).is_err());
}

#[test]
fn admissions_are_immutable_and_name_and_map_key_checks_are_enforced() {
    let founder = SecretKey::generate();
    let second = SecretKey::generate();
    let mut base = initial(&founder);
    add(
        &mut base,
        member(&founder, second.public(), "second", true, 2),
    );
    let mut state = boot(&base);
    let admission = base.admissions[&second.public().to_string()].clone();
    base.admissions.insert(
        second.public().to_string(),
        member(&second, second.public(), "replacement", true, 3),
    );
    base.members.insert(
        second.public().to_string(),
        member(&founder, second.public(), "BAD", true, 3),
    );
    base.members.insert(
        "wrong-key".into(),
        member(&founder, second.public(), "second", true, 4),
    );
    assert_eq!(merge(&mut state, &founder, &base).rejected, 2);
    assert_eq!(
        state.snapshot().admissions[&second.public().to_string()],
        admission
    );
    assert_eq!(
        state.snapshot().members[&second.public().to_string()]
            .body
            .person,
        "second"
    );
}
