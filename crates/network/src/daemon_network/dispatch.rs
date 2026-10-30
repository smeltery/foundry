use super::{
    driver::{Live, retire},
    *,
};
use std::collections::{BTreeSet, HashSet};
pub(super) async fn batch(
    state: &State,
    outbox: &outbox::Outbox,
    live: &mut BTreeMap<EndpointId, Live>,
) -> Result<HashSet<EndpointId>, ControlError> {
    let Some(view) = state.view().await? else {
        suspend(state, outbox, live).await?;
        return Ok(HashSet::new());
    };
    let revision = view.revision;
    state.engine.sync(view, None).await?;
    let presence = state
        .engine
        .with(|engine| engine.local_presence())
        .await
        .map_err(|_| ControlError::Storage)?;
    let mut failed = Vec::new();
    for (peer, connection) in &mut *live {
        if connection.published != Some(revision) {
            if connection.sender.publish_state().await.is_err() {
                failed.push(*peer);
                continue;
            }
            connection.published = Some(revision);
        }
        if connection.presence.as_ref() != Some(&presence) {
            if connection
                .sender
                .publish_presence(presence.clone())
                .await
                .is_err()
            {
                failed.push(*peer);
                continue;
            }
            connection.presence = Some(presence.clone());
        }
    }
    // Finish failed advertisement cleanup before any durable send claim exists.
    for peer in std::mem::take(&mut failed) {
        if let Some(old) = live.remove(&peer) {
            retire(state, outbox, old, peer).await?;
        }
    }
    let Some(view) = state.view().await? else {
        suspend(state, outbox, live).await?;
        return Ok(HashSet::new());
    };
    let members = view
        .members
        .iter()
        .filter(|m| m.active && m.endpoint != view.local)
        .map(|m| m.endpoint)
        .collect::<HashSet<_>>();
    // The complete global preflight and all attempt claims share a fresh actual
    // authority guard. After this point, wire writes have no metadata dependency.
    crate::daemon_outbox::diagnostic(format_args!("batch_enter live={}", live.len()));
    let batch = outbox
        .batch(
            view,
            live.iter().map(|(peer, c)| (*peer, c.generation)).collect(),
        )
        .await?;
    crate::daemon_outbox::diagnostic(format_args!(
        "batch_done send={} retire={}",
        batch.messages.len(),
        batch.retire.len()
    ));
    let retired = batch
        .retire
        .into_iter()
        .map(|(peer, _)| peer)
        .collect::<BTreeSet<_>>();
    for peer in retired {
        if let Some(old) = live.remove(&peer) {
            retire(state, outbox, old, peer).await?;
        }
    }
    for (peer, generation, message) in batch.messages {
        if failed.contains(&peer) {
            continue;
        }
        if let Some(connection) = live.get(&peer)
            && connection.generation == generation
            && connection.sender.send_text(*message).await.is_err()
        {
            failed.push(peer);
        }
    }
    for peer in failed {
        if let Some(old) = live.remove(&peer) {
            retire(state, outbox, old, peer).await?;
        }
    }
    if state
        .settings
        .code_policy()
        .map_err(|_| ControlError::Invalid)?
        .is_some()
    {
        for effect in state
            .authority
            .code_effects()
            .await
            .map_err(|_| ControlError::Storage)?
        {
            if matches!(&effect,crate::full_team_store::ControlEffect::PublishLatestState{revision:expected,..}
                if *expected<=revision && live.values().all(|connection|connection.published.is_some_and(|sent|sent>=*expected)))
            {
                match state.authority.acknowledge_code_effect(effect).await {
                    Ok(()) | Err(crate::peer_session::PeerError::StaleEpoch) => {}
                    Err(_) => return Err(ControlError::Storage),
                }
            }
        }
    }
    match state.codes.reconcile(state).await? {
        codes::Reconciled::Current | codes::Reconciled::Retry => {}
        codes::Reconciled::Quiescent => {
            suspend(state, outbox, live).await?;
            return Ok(HashSet::new());
        }
    }
    Ok(members)
}
async fn suspend(
    state: &State,
    outbox: &outbox::Outbox,
    live: &mut BTreeMap<EndpointId, Live>,
) -> Result<(), ControlError> {
    for (peer, connection) in std::mem::take(live) {
        retire(state, outbox, connection, peer).await?;
    }
    state.codes.shutdown().await?;
    state
        .engine
        .with(|engine| {
            engine.suspend_remote_directory();
            Ok(())
        })
        .await
        .map_err(|_| ControlError::Storage)
}
