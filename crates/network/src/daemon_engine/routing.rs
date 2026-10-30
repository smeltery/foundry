use super::*;
use crate::wire;
use iroh::EndpointId;

pub(super) struct Route {
    pub entry: Entry,
    pub remote: Option<(String, EndpointId)>,
}
fn matches(filter: &Filter, entry: &Entry) -> bool {
    filter
        .person
        .as_ref()
        .is_none_or(|p| p == &entry.conversation.person)
        && filter
            .machine
            .as_ref()
            .is_none_or(|p| p == &entry.conversation.machine)
        && filter
            .name
            .as_ref()
            .is_none_or(|p| p == &entry.conversation.name)
        && filter
            .repository
            .as_ref()
            .is_none_or(|p| entry.repository.as_ref() == Some(p))
        && (!filter.online_only || entry.online)
}
impl Engine {
    pub(super) fn directory_routes(&self, filter: &Filter) -> Result<Vec<Route>, EngineError> {
        let mut local_filter = filter.clone();
        local_filter.repository = None;
        let local = self
            .registry
            .registry()?
            .who(&local_filter.native(), registry::MAX_RESULTS)?;
        let mut routes: Vec<Route> = local
            .into_iter()
            .filter(|entry| {
                filter.repository.as_ref().is_none_or(|path| {
                    entry.repository.as_ref().is_some_and(|repo| {
                        repo == path || std::fs::canonicalize(path).ok().as_ref() == Some(repo)
                    })
                })
            })
            .map(|entry| Route {
                entry: entry.into(),
                remote: None,
            })
            .collect();
        if self.remote_ready
            && let Some(snapshot) = self.mail.remote_snapshot()?
        {
            for advertised in &snapshot.presence {
                let member = snapshot
                    .members
                    .iter()
                    .find(|m| m.peer == advertised.peer)
                    .ok_or(EngineError::Schema)?;
                let mut names = std::collections::BTreeSet::new();
                for session in &advertised.presence.sessions {
                    remote::label(&session.name)?;
                    names.insert(session.name.clone());
                    let online = advertised.online
                        && self.remote_generations.get(&advertised.peer)
                            == Some(&(advertised.generation, true))
                        && session.tier != wire::DeliveryTier::None;
                    let entry = Entry {
                        conversation: Conversation {
                            person: member.person.clone(),
                            machine: member.machine.clone(),
                            name: session.name.clone(),
                        },
                        repository: session.repo.as_ref().map(std::path::PathBuf::from),
                        online,
                        status: if advertised.online {
                            session.status.clone()
                        } else {
                            "offline".into()
                        },
                        doing: session.doing.clone().unwrap_or_default(),
                        capabilities: match session.tier {
                            wire::DeliveryTier::Pull => vec![Capability::Pull],
                            wire::DeliveryTier::Push => vec![Capability::Push],
                            wire::DeliveryTier::Turn => vec![Capability::Hook],
                            wire::DeliveryTier::None => Vec::new(),
                        },
                    };
                    if matches(filter, &entry) {
                        routes.push(Route {
                            entry,
                            remote: Some((snapshot.team.clone(), member.peer)),
                        });
                    }
                }
                for value in advertised
                    .presence
                    .known
                    .iter()
                    .chain(&advertised.presence.kept)
                {
                    let name = remote::known_name(value, member)?;
                    if !names.insert(name.clone()) {
                        continue;
                    }
                    let entry = Entry {
                        conversation: Conversation {
                            person: member.person.clone(),
                            machine: member.machine.clone(),
                            name,
                        },
                        repository: None,
                        online: false,
                        status: "offline".into(),
                        doing: String::new(),
                        capabilities: Vec::new(),
                    };
                    if matches(filter, &entry) {
                        routes.push(Route {
                            entry,
                            remote: Some((snapshot.team.clone(), member.peer)),
                        });
                    }
                }
            }
        }
        if routes.len() > registry::MAX_RESULTS {
            return Err(EngineError::Limit);
        }
        Ok(routes)
    }
    pub(super) fn resolve_route(&self, filter: &Filter) -> Result<Route, EngineError> {
        let mut routes = self.directory_routes(filter)?;
        // Prefer a live destination over retained offline advertisements, as
        // the transport directory does, without concealing live ambiguity.
        if routes.iter().any(|r| r.entry.online) {
            routes.retain(|r| r.entry.online);
        }
        // A person's unqualified terminal prefers their actual local terminal.
        if filter.machine.is_none()
            && routes.len() > 1
            && routes.iter().all(|r| {
                r.entry.conversation.name == "terminal"
                    && r.entry.conversation.person == routes[0].entry.conversation.person
            })
            && routes.iter().any(|r| r.remote.is_none())
        {
            routes.retain(|r| r.remote.is_none());
        }
        if routes.len() != 1 {
            return Err(if routes.is_empty() {
                EngineError::NotFound
            } else {
                EngineError::Ambiguous
            });
        }
        Ok(routes.remove(0))
    }
    pub(super) fn send_remote(
        &mut self,
        auth: &Auth,
        message: &mail::Message,
        recipient: &Conversation,
        team: &str,
        peer: EndpointId,
    ) -> Result<Outcome, EngineError> {
        if message.body.len() > wire::MAX_PEER_TEXT_BYTES {
            return Err(EngineError::InvalidPayload);
        }
        let sender_filter = registry::DirectoryFilter {
            person: Some(auth.conversation.person.clone()),
            machine: Some(auth.conversation.machine.clone()),
            name: Some(auth.conversation.name.clone()),
            ..Default::default()
        };
        let sender = self
            .registry
            .registry()?
            .who(&sender_filter, 1)?
            .into_iter()
            .next()
            .ok_or(EngineError::Unauthorized)?;
        let message = wire::Message {
            id: message.id.clone(),
            from: remote::address(&auth.conversation),
            from_agent: None,
            from_repo: sender.repository.map(|p| p.to_string_lossy().into_owned()),
            to: remote::address(recipient),
            via: None,
            reply_to: message.reply_to.clone(),
            body: String::from_utf8(message.body.clone())
                .map_err(|_| EngineError::InvalidPayload)?,
            wake: message.intent.wake,
            notify: message.intent.notify,
            sent_at: self.now()?,
            approval: None,
            files: Vec::new(),
        };
        let queued = self.mail.enqueue_transport(team, peer, &message)?;
        Ok(Outcome::TransportQueued {
            message_id: queued.message.id,
            peer: peer.to_string(),
            revision: queued.revision,
        })
    }
}
