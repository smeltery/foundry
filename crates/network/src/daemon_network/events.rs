use super::{
    driver::{Event, Live, retire},
    *,
};
use crate::peer_session::{DriverEvent, PeerEvent};
pub(super) async fn handle(
    state: &State,
    outbox: &outbox::Outbox,
    live: &mut BTreeMap<EndpointId, Live>,
    event: Event,
) -> Result<(), ControlError> {
    let Event {
        peer,
        generation,
        event,
    } = event;
    if live
        .get(&peer)
        .is_none_or(|connection| connection.generation != generation)
    {
        return Ok(());
    }
    let mut close = event.is_none();
    match event {
        Some(DriverEvent::Warning(warning)) => {
            eprintln!(
                "network protocol version mismatch: expected {}, observed {}",
                warning.expected, warning.observed
            );
        }
        Some(DriverEvent::Peer(PeerEvent::Receipt {
            message_id,
            disposition,
            note,
        })) => {
            // The permit-bound publisher owns the accepted work. This event
            // wakes its guarded drain; a batch may already have settled it.
            let _receipt = (message_id, disposition, note);
            if let Some(view) = state.view().await? {
                close = outbox.drain_receipts(view, peer, generation).await?;
            } else {
                close = true;
            }
        }
        Some(DriverEvent::Peer(PeerEvent::Presence {
            peer: actual,
            presence,
        })) => {
            if actual != peer {
                close = true;
            } else if let Some(view) = state.view().await? {
                if view
                    .members
                    .iter()
                    .any(|member| member.active && member.endpoint == peer)
                {
                    state
                        .engine
                        .sync(view, Some((peer, generation, presence)))
                        .await?;
                } else {
                    close = true;
                }
            } else {
                close = true;
            }
        }
        Some(DriverEvent::Peer(_)) => state.wake.notify_one(),
        None => {}
    }
    if close && let Some(old) = live.remove(&peer) {
        retire(state, outbox, old, peer).await?;
    }
    Ok(())
}

// Keep the last validated snapshot as offline metadata. Both operations share
// one Engine lock, so a directory request cannot observe an online resurrection.
pub(super) async fn retired_presence(
    state: &State,
    peer: EndpointId,
    generation: u64,
    presence: Option<crate::wire::Presence>,
) -> Result<(), ControlError> {
    let view = state.view().await?;
    state
        .engine
        .with(move |engine| {
            if let Some(view) = &view {
                let members = view
                    .members
                    .iter()
                    .filter(|member| member.active && member.endpoint != view.local)
                    .map(|member| crate::daemon_engine::RemoteMember {
                        peer: member.endpoint,
                        person: member.person.clone(),
                        machine: member.machine.clone(),
                    })
                    .collect::<Vec<_>>();
                engine.sync_remote_members(&view.team, view.revision, &members)?;
                if members.iter().any(|member| member.peer == peer)
                    && let Some(presence) = presence
                {
                    engine.remote_presence(peer, generation, &presence)?;
                }
            } else {
                engine.suspend_remote_directory();
            }
            engine.remote_disconnected(peer, generation)
        })
        .await
        .map_err(|_| ControlError::Storage)
}
