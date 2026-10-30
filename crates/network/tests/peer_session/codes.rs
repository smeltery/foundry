use super::*;
use cap_std::{ambient_authority, fs::Dir};
use foundry_network::{
    code_invite::{CodePolicy, Codebook},
    full_team_store::{CodeLease, ControlEffect, StorePolicy},
    team::JoinRequest,
    ticket::TicketCodec,
};
use std::sync::atomic::{AtomicI64, Ordering};

pub(super) fn policy() -> CodePolicy {
    // Original synthetic fixture, not an imported dictionary.
    let bytes = b"testwordalpha\ntestwordbeta\n";
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .try_into()
        .unwrap();
    CodePolicy::new(Codebook::from_bytes(bytes, digest).unwrap(), &profile()).unwrap()
}
fn open_codes(node: &Node) -> full_team_store::FullTeamStore {
    full_team_store::FullTeamStore::open_existing(
        &node.root,
        node.identity.clone(),
        StorePolicy::new(profile(), None)
            .unwrap()
            .with_codes(policy())
            .unwrap(),
        WAIT,
    )
    .unwrap()
}
pub(super) fn authority(node: &Node, clock: Arc<AtomicI64>) -> Arc<AdmittedTeam> {
    let store = node.store().enable_codes(policy(), NOW + 10).unwrap();
    let directory = Dir::open_ambient_dir(&node.root, ambient_authority()).unwrap();
    let lease = daemon_lease::DaemonLease::acquire(&directory).unwrap();
    AdmittedTeam::new(
        store,
        lease,
        node.identity.endpoint_id(),
        move || Ok(clock.load(Ordering::SeqCst)),
        None,
    )
    .unwrap()
}
async fn issued(team: &AdmittedTeam) -> (String, Arc<CodeLease>) {
    let issued = team.issue_code(1, false).await.unwrap();
    let ticket = TicketCodec::new(&profile(), None)
        .unwrap()
        .decode(issued.issued().ticket())
        .unwrap();
    assert!(issued.issued().listener_intent().is_some());
    assert!(!issued.issued().code().is_empty());
    drop(issued);
    (
        ticket.secret,
        team.recover_code_intents().await.unwrap().pop().unwrap(),
    )
}
pub(super) fn join_request(team: &str, secret: String) -> JoinRequest {
    JoinRequest {
        team: team.into(),
        secret,
        person: "new-peer".into(),
        machine: "desktop".into(),
        proof: None,
        owner_only: false,
    }
}
#[tokio::test]
async fn code_admission_is_exact_bearer_bound_consumed_and_effect_ack_is_generation_safe() {
    let (a, b, newcomer) = (Node::new(), Node::new(), Node::new());
    provision(&a, &b);
    let team = a.store().load(NOW).unwrap().document().unwrap().id.clone();
    let authority = authority(&a, Arc::new(AtomicI64::new(NOW + 100)));
    let (secret, lease) = issued(&authority).await;
    let effects = authority.code_effects().await.unwrap();
    let old = effects
        .into_iter()
        .find(|e| matches!(e, ControlEffect::ReconcileCode { .. }))
        .unwrap();
    let permit = authority.code_permit(lease.clone()).await.unwrap();
    assert_eq!(permit.derived().bearer_secret(), secret);
    assert_eq!(permit.lease().id(), lease.id());
    assert!(permit.lease().expires_ms() > NOW + 100);
    assert_eq!(permit.lease().generation(), lease.generation());
    assert!(
        tokio::time::timeout(
            Duration::from_millis(40),
            authority.remove(vec![b.identity.endpoint_id()])
        )
        .await
        .is_err()
    );
    drop(permit);
    let bad = join_request(&team, "another-code-bearer".into());
    assert!(matches!(
        authority
            .admit_code_join(newcomer.identity.endpoint_id(), bad, lease.clone())
            .await,
        Err(PeerError::Unauthorized)
    ));
    let request = join_request(&team, secret);
    let admission = authority
        .admit_code_join(
            newcomer.identity.endpoint_id(),
            request.clone(),
            lease.clone(),
        )
        .await
        .unwrap();
    assert_eq!(admission.peer(), newcomer.identity.endpoint_id());
    assert!(
        admission.state().members[&newcomer.identity.endpoint_id().to_string()]
            .body
            .active
    );
    drop(admission);
    assert!(authority.recover_code_intents().await.unwrap().is_empty());
    assert!(matches!(
        authority.code_permit(lease).await,
        Err(PeerError::StaleEpoch)
    ));
    assert_eq!(
        authority.acknowledge_code_effect(old).await,
        Err(PeerError::StaleEpoch)
    );
    let effects = authority.code_effects().await.unwrap();
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, ControlEffect::RetireCode { .. }))
    );
    let effect = effects[0].clone();
    authority
        .acknowledge_code_effect(effect.clone())
        .await
        .unwrap();
    assert!(!authority.code_effects().await.unwrap().contains(&effect));
    // Ordinary same-peer long-ticket retry retains the invitation model even
    // though the consumed code lease can no longer bind a new endpoint.
    drop(
        authority
            .admit_join(newcomer.identity.endpoint_id(), request)
            .await
            .unwrap(),
    );
    assert!(authority.refresh().await.is_ok());
}
#[tokio::test]
async fn code_expiry_and_foreign_store_lease_refuse_material_without_poisoning_live_gate() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let clock = Arc::new(AtomicI64::new(NOW + 100));
    let first = authority(&a, clock.clone());
    let second = authority(&b, Arc::new(AtomicI64::new(NOW + 100)));
    let (_, lease) = issued(&first).await;
    assert!(matches!(
        second.code_permit(lease.clone()).await,
        Err(PeerError::Unauthorized)
    ));
    clock.store(lease.expires_ms(), Ordering::SeqCst);
    assert!(first.recover_code_intents().await.unwrap().is_empty());
    assert!(matches!(
        first.code_permit(lease).await,
        Err(PeerError::StaleEpoch)
    ));
    assert!(first.refresh().await.is_ok());
}
#[tokio::test]
async fn issued_publication_permit_blocks_removal_and_durable_reopen_keeps_intent() {
    let (a, b) = (Node::new(), Node::new());
    provision(&a, &b);
    let gate = authority(&a, Arc::new(AtomicI64::new(NOW + 100)));
    let issued = gate.issue_code(2, true).await.unwrap();
    let intent = issued.issued().listener_intent().unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(40),
            gate.remove(vec![b.identity.endpoint_id()])
        )
        .await
        .is_err()
    );
    drop(issued);
    drop(gate);
    let directory = Dir::open_ambient_dir(&a.root, ambient_authority()).unwrap();
    let lease = daemon_lease::DaemonLease::acquire(&directory).unwrap();
    let reopened = AdmittedTeam::new(
        open_codes(&a),
        lease,
        a.identity.endpoint_id(),
        || Ok(NOW + 100),
        None,
    )
    .unwrap();
    assert_eq!(
        reopened.recover_code_intents().await.unwrap()[0].id(),
        intent
    );
}

#[tokio::test]
async fn empty_team_is_code_only_and_nonempty_mismatch_is_never_overridden() {
    let (a, b, newcomer) = (Node::new(), Node::new(), Node::new());
    provision(&a, &b);
    let gate = authority(&a, Arc::new(AtomicI64::new(NOW + 100)));
    let (secret, lease) = issued(&gate).await;
    assert!(matches!(
        gate.admit_join(
            newcomer.identity.endpoint_id(),
            join_request("", secret.clone())
        )
        .await,
        Err(PeerError::Limit)
    ));
    assert!(matches!(
        gate.admit_code_join(
            newcomer.identity.endpoint_id(),
            join_request("other-team", secret),
            lease.clone()
        )
        .await,
        Err(PeerError::DifferentTeam)
    ));
    assert_eq!(gate.recover_code_intents().await.unwrap().len(), 1);
}
