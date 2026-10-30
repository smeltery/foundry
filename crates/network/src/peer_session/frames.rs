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
    if value.body.len() > wire::MAX_PEER_TEXT_BYTES || value.sent_at < 0 {
        return Err(PeerError::Limit);
    }
    if !value.files.is_empty() || value.approval.is_some() {
        return Err(PeerError::Unsupported);
    }
    serde_json::to_writer(MessageSize(0), value).map_err(|_| PeerError::Limit)?;
    Ok(())
}

struct MessageSize(usize);
impl std::io::Write for MessageSize {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > crate::codec::MAX_FRAME_BYTES.saturating_sub(self.0) {
            return Err(std::io::Error::other("message exceeds frame budget"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Text annotations are descriptive, never transport authority. Keep accepted
/// characters in order, bound their output, and turn empty annotations into None.
pub(super) fn clean_metadata(value: &mut wire::Message) {
    fn annotation(value: &mut Option<String>, punctuation: &str, limit: usize) {
        if let Some(input) = value.take() {
            let cleaned: String = input
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || punctuation.contains(*c))
                .take(limit)
                .collect();
            *value = (!cleaned.is_empty()).then_some(cleaned);
        }
    }
    annotation(&mut value.from_agent, "-", 30);
    annotation(&mut value.from_repo, "-_./", 100);
    annotation(&mut value.via, "-_./:*@", 100);
    annotation(&mut value.reply_to, "", 40);
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
                let id = msg.id.clone();
                let received = io::bounded(
                    &mut self.options.cancel,
                    self.options.deadline,
                    self.team.receive_text(self.peer, *msg),
                )
                .await?;
                if let Err(error @ (PeerError::Storage | PeerError::Quiescent)) = &received {
                    return Err(*error);
                }
                // Revalidate even after a payload/policy error. A revoked or
                // storage-fenced authority must not emit any new receipt.
                let _permit = self.permit().await?;
                let (state, note, event) = match received {
                    Ok((id, duplicate)) => (
                        "queued",
                        String::new(),
                        PeerEvent::Received {
                            message_id: id,
                            duplicate,
                        },
                    ),
                    Err(error) => {
                        let note = error.to_string();
                        (
                            "refused",
                            note.clone(),
                            PeerEvent::Rejected {
                                message_id: id.clone(),
                                note,
                            },
                        )
                    }
                };
                io::write(
                    &mut self.send,
                    &PeerFrame::Ack {
                        id: id.clone(),
                        state: state.into(),
                        note,
                    },
                    &mut self.options,
                )
                .await?;
                Ok(event)
            }
            PeerFrame::Ack { id, state, note } => {
                let permit = self.permit().await?;
                let disposition =
                    ReceiptDisposition::from_state(&state).map_err(|_| PeerError::Limit)?;
                let revision = self.pending.remove(&id).ok_or(PeerError::UnexpectedAck)?;
                if revision != permit.revision {
                    return Err(PeerError::StaleEpoch);
                }
                if let Some(publish) = &self.receipt_publisher {
                    publish(&id, &disposition, revision)?;
                }
                // Registration and the expiry scan use this same gate. The
                // validated work is visible before a scan can decide retirement.
                drop(permit);
                Ok(PeerEvent::Receipt {
                    message_id: id,
                    disposition,
                    note,
                })
            }
            PeerFrame::Hello { .. } => Err(PeerError::Protocol),
        }
    }
}
