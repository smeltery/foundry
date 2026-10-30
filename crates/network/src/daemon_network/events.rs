use super::{
    driver::{Event, Live, retire},
    *,
};
use crate::{
    daemon_outbox::Settlement,
    peer_session::{DriverEvent, PeerEvent},
};
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
    crate::daemon_outbox::diagnostic(format_args!(
        "event_seen generation={} receipt={} live={:?}",
        generation,
        matches!(&event, Some(DriverEvent::Peer(PeerEvent::Receipt { .. }))),
        live.get(&peer).map(|v| v.generation)
    ));
    if live
        .get(&peer)
        .is_none_or(|connection| connection.generation != generation)
    {
        return Ok(());
    }
    crate::daemon_outbox::diagnostic(format_args!(
        "event generation={} receipt={} present={}",
        generation,
        matches!(&event, Some(DriverEvent::Peer(PeerEvent::Receipt { .. }))),
        event.is_some()
    ));
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
            let _note = note; // Preserve the raw bounded field without logging it.
            if let Some(view) = state.view().await? {
                match outbox
                    .receipt(view, peer, generation, message_id, disposition)
                    .await?
                {
                    Settlement::Retire => close = true,
                    Settlement::Settled { taken } => {
                        let _taken = taken;
                    }
                }
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
