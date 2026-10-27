use super::tests::{Fixture, add, seal};
use super::*;
use iroh::SecretKey;
const NOW: i64 = 1_000_000;
#[test]
fn proof_commit_uses_actual_clock_not_retained_horizon() {
    let f = Fixture::new();
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../team_state_support/proofs.json")).unwrap();
    let jwks = serde_json::to_vec(&fixture["jwks"]).unwrap();
    let policy = StorePolicy::new(
        f.profile.clone(),
        Some((
            ProofConfig {
                issuer: "team-issuer".into(),
                audience: "team-audience".into(),
                token_type: "team-proof".into(),
            },
            &jwks,
        )),
    )
    .unwrap();
    let mut store =
        FullTeamStore::create_new(&f.root, f.identity.clone(), policy, Duration::from_secs(1))
            .unwrap();
    let state = f.formation();
    let peer = SecretKey::from_bytes(&[2; 32]);
    let state = add(
        &state,
        f.identity.secret_key(),
        &peer,
        "alex",
        &state.document().link.as_ref().unwrap().body.secret,
    );
    seal(&store, state.document().clone());
    let base = store.load(NOW).unwrap();
    let pending = store
        .prepare_attach_proof(
            &base,
            peer.public(),
            fixture["tokens"]["future"].as_str().unwrap(),
            100_000_000,
        )
        .unwrap();
    assert!(matches!(
        store.commit_transition(pending, NOW),
        Err(StoreError::Corrupt)
    ));
    assert!(
        store
            .load(NOW)
            .unwrap()
            .document()
            .unwrap()
            .attestations
            .is_empty()
    );
    let pending = store
        .prepare_attach_proof(
            &base,
            peer.public(),
            fixture["tokens"]["owner"].as_str().unwrap(),
            NOW,
        )
        .unwrap();
    match store.commit_transition(pending, NOW).unwrap() {
        CommitOutcome::Active(state) => assert_eq!(state.document().unwrap().attestations.len(), 1),
        _ => panic!("proof not stored"),
    }
}
#[test]
fn same_timestamp_signature_losing_removal_is_not_quiescence() {
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
    let id = f.identity.endpoint_id().to_string();
    let record = state.document().members[&id].body.clone();
    let variants = |active| {
        (0..32)
            .map(|n| {
                let mut body = record.clone();
                body.active = active;
                body.ts = NOW + 1;
                body.machine = format!("desk-{n}");
                crate::signing::SignedRecord::sign(body, &peer, &state.document().id, &f.profile)
                    .unwrap()
            })
            .collect::<Vec<_>>()
    };
    let strongest = variants(true)
        .into_iter()
        .max_by(|a, b| a.sig.cmp(&b.sig))
        .unwrap();
    let losing = variants(false)
        .into_iter()
        .min_by(|a, b| a.sig.cmp(&b.sig))
        .unwrap();
    assert!(losing.sig < strongest.sig);
    let mut updated = state.document().clone();
    updated.members.insert(id.clone(), strongest);
    let (state, report) = state.merge_from(peer.public(), &updated, NOW + 1).unwrap();
    assert_eq!(report.rejected, 0);
    seal(&store, state.document().clone());
    let base = store.load(NOW + 1).unwrap();
    let mut incoming = state.document().clone();
    incoming.members.insert(id, losing);
    let (pending, _) = store
        .prepare_merge(&base, peer.public(), &incoming, NOW + 1)
        .unwrap();
    assert!(!pending.requires_quiescence());
    assert!(matches!(
        store.commit_transition(pending, NOW + 1).unwrap(),
        CommitOutcome::Unchanged(_)
    ));
}
#[test]
fn validated_removal_at_revision_ceiling_preserves_quiescence_without_publication() {
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
    let (mut checkpoint, _) =
        checkpoint::read(&store.connection, &store.identity, &store.policy).unwrap();
    checkpoint.revision = i64::MAX as u64;
    checkpoint::write(&store.connection, &checkpoint, &store.identity, false).unwrap();
    let base = store.load(NOW).unwrap();
    let bytes = || {
        store
            .connection
            .query_row("SELECT payload,signature FROM checkpoint", [], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .unwrap()
    };
    let before = bytes();
    assert!(matches!(
        store.prepare_rotation(&base, NOW + 1),
        Err(StoreError::RevisionOverflow)
    ));
    let local = f.identity.endpoint_id().to_string();
    let mut incoming = state.document().clone();
    let mut body = incoming.members[&local].body.clone();
    body.active = false;
    body.ts = NOW + 1;
    incoming.members.insert(
        local.clone(),
        crate::signing::SignedRecord::sign(body.clone(), &peer, &incoming.id, &f.profile).unwrap(),
    );
    let (pending, _) = store
        .prepare_merge(&base, peer.public(), &incoming, NOW + 1)
        .unwrap();
    let quiesce = pending.requires_quiescence();
    assert!(quiesce);
    assert!(matches!(
        store.commit_transition(pending, NOW + 1),
        Err(StoreError::RevisionOverflow)
    ));
    assert!(quiesce);
    let departure = store.prepare_departure(&base, NOW + 1).unwrap();
    let quiesce = departure.requires_quiescence();
    assert!(quiesce);
    assert!(matches!(
        store.commit_transition(departure, NOW + 1),
        Err(StoreError::RevisionOverflow)
    ));
    assert!(quiesce);
    incoming.members.get_mut(&local).unwrap().sig = "forged".into();
    let (forged, report) = store
        .prepare_merge(&base, peer.public(), &incoming, NOW + 1)
        .unwrap();
    assert!(!forged.requires_quiescence());
    assert!(report.rejected > 0);
    assert!(matches!(
        store.commit_transition(forged, NOW + 1).unwrap(),
        CommitOutcome::Unchanged(_)
    ));
    body.ts = NOW - 1;
    incoming.members.insert(
        local,
        crate::signing::SignedRecord::sign(body, &peer, &incoming.id, &f.profile).unwrap(),
    );
    let (stale, _) = store
        .prepare_merge(&base, peer.public(), &incoming, NOW + 1)
        .unwrap();
    assert!(!stale.requires_quiescence());
    assert!(matches!(
        store.commit_transition(stale, NOW + 1).unwrap(),
        CommitOutcome::Unchanged(_)
    ));
    let after: (Vec<u8>, Vec<u8>) = store
        .connection
        .query_row("SELECT payload,signature FROM checkpoint", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(after, before);
    let loaded = store.load(NOW + 1).unwrap();
    assert_eq!(loaded.revision(), i64::MAX as u64);
    assert!(loaded.document() == base.document());
}
