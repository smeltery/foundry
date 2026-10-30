use super::*;
use crate::{signing::MemberRecord, team::TeamDocument};
use std::collections::BTreeSet;

pub(super) fn encode_state(document: &TeamDocument) -> Result<PeerFrame> {
    // Only the admission-gated store snapshot reaches this sink. Active recipients
    // receive the full committed state; unknown/inactive recipients receive none.
    let value = serde_json::to_value(document).map_err(|_| PeerError::Invalid)?;
    let state = serde_json::from_value(value).map_err(|_| PeerError::Invalid)?;
    Ok(PeerFrame::State {
        state: Box::new(state),
    })
}
pub(super) fn decode_state(state: wire::StateDocument) -> Result<TeamDocument> {
    let value = serde_json::to_value(state).map_err(|_| PeerError::Invalid)?;
    serde_json::from_value(value).map_err(|_| PeerError::Invalid)
}
fn text(value: &str, maximum: usize, empty: bool) -> Result<()> {
    if (!empty && value.is_empty()) || value.len() > maximum || value.chars().any(char::is_control)
    {
        return Err(PeerError::Limit);
    }
    Ok(())
}
fn address(value: &str, owner: &MemberRecord) -> Result<()> {
    text(value, 256, false)?;
    let prefix = format!("{}/{}/", owner.person, owner.machine);
    let name = value.strip_prefix(&prefix).ok_or(PeerError::Unauthorized)?;
    text(name, 128, false)?;
    if name.contains('/') {
        return Err(PeerError::Unauthorized);
    }
    Ok(())
}
/// This neutral profile scopes addresses as person/machine/conversation. Remote
/// person/machine fields must match the transport endpoint's durable roster row.
pub(super) fn presence(value: &mut wire::Presence, owner: &MemberRecord) -> Result<()> {
    if value.sessions.len() > MAX_PRESENCE
        || value.known.len() > MAX_PRESENCE
        || value.kept.len() > MAX_PRESENCE
        || value.features.len() > 16
    {
        return Err(PeerError::Limit);
    }
    let mut names = BTreeSet::new();
    for session in &mut value.sessions {
        address(&session.address, owner)?;
        text(&session.name, 128, false)?;
        if session.person != owner.person
            || session.machine != owner.machine
            || session.address != format!("{}/{}/{}", owner.person, owner.machine, session.name)
            || !names.insert(&session.address)
        {
            return Err(PeerError::Unauthorized);
        }
        for field in [&session.agent, &session.cwd, &session.status] {
            text(field, 512, true)?;
        }
        for field in [&session.repo, &session.branch, &session.doing]
            .into_iter()
            .flatten()
        {
            text(field, 512, true)?;
        }
        session.is_me = false;
    }
    for field in value.known.iter().chain(&value.kept) {
        address(field, owner)?;
    }
    for feature in &value.features {
        text(feature, 64, false)?;
    }
    Ok(())
}
pub(super) fn message(
    value: &wire::Message,
    sender: &MemberRecord,
    recipient: &MemberRecord,
) -> Result<()> {
    address(&value.from, sender)?;
    address(&value.to, recipient)?;
    text(&value.id, 256, false)?;
    if let Some(reply) = &value.reply_to {
        text(reply, 256, false)?;
    }
    if value.body.len() > 128 * 1024 || value.sent_at < 0 {
        return Err(PeerError::Limit);
    }
    if !value.files.is_empty()
        || value.approval.is_some()
        || value.via.is_some()
        || value.from_agent.is_some()
        || value.from_repo.is_some()
    {
        return Err(PeerError::Unsupported);
    }
    // Preserve the entire accepted wire message in the durable receiver record.
    if serde_json::to_vec(value)
        .map_err(|_| PeerError::Invalid)?
        .len()
        > 256 * 1024
    {
        return Err(PeerError::Limit);
    }
    Ok(())
}

impl PeerSession {
    pub(super) async fn handle_frame(&mut self, frame: PeerFrame) -> Result<PeerEvent> {
        match frame {
            PeerFrame::State { state } => {
                let revision = self.merge_state(frames::decode_state(*state)?).await?;
                Ok(PeerEvent::StateCommitted { revision })
            }
            PeerFrame::Presence { mut presence } => {
                let permit = self.permit().await?;
                frames::presence(&mut presence, &permit.remote)?;
                Ok(PeerEvent::Presence {
                    peer: self.peer,
                    presence,
                })
            }
            PeerFrame::Msg { msg } => {
                let (id, duplicate) = io::bounded(
                    &mut self.options.cancel,
                    self.options.deadline,
                    self.team.receive_text(self.peer, *msg),
                )
                .await??;
                let _permit = self.permit().await?;
                io::write(
                    &mut self.send,
                    &PeerFrame::Ack {
                        id: id.clone(),
                        state: "queued".into(),
                        note: String::new(),
                    },
                    &mut self.options,
                )
                .await?;
                Ok(PeerEvent::Received {
                    message_id: id,
                    duplicate,
                })
            }
            PeerFrame::Ack { id, state, note } => {
                let permit = self.permit().await?;
                if state != "queued" || !note.is_empty() {
                    return Err(PeerError::Unsupported);
                }
                let revision = self.pending.remove(&id).ok_or(PeerError::UnexpectedAck)?;
                if revision != permit.revision {
                    return Err(PeerError::StaleEpoch);
                }
                Ok(PeerEvent::Queued { message_id: id })
            }
            PeerFrame::Hello { .. } => Err(PeerError::Protocol),
        }
    }
}
