use super::*;
use crate::wire;
use iroh::EndpointId;

#[cfg(test)]
#[path = "remote_presence_tests.rs"]
mod presence_tests;

pub(super) fn label(value: &str) -> Result<(), EngineError> {
    if value.is_empty()
        || value.len() > registry::MAX_TEXT_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(EngineError::InvalidInput);
    }
    Ok(())
}
pub(super) fn address(conversation: &Conversation) -> String {
    format!(
        "{}/{}/{}",
        conversation.person, conversation.machine, conversation.name
    )
}
pub(super) fn known_name(value: &str, owner: &RemoteMember) -> Result<String, EngineError> {
    let prefix = format!("{}/{}/", owner.person, owner.machine);
    let name = value.strip_prefix(&prefix).unwrap_or(value);
    label(name)?;
    if value.contains('/') && !value.starts_with(&prefix) {
        return Err(EngineError::Unauthorized);
    }
    Ok(name.into())
}
impl Engine {
    pub fn authorize_control(
        &self,
        connection: ConnectionId,
        auth: &Auth,
    ) -> Result<(), EngineError> {
        if self.uncertain {
            return Err(EngineError::StorageUncertain);
        }
        self.authorize(connection, auth)
    }
    pub(super) fn remote_result<T>(&mut self, result: mail::Result<T>) -> Result<T, EngineError> {
        let result = result.map_err(EngineError::from);
        if matches!(result, Err(EngineError::StorageUncertain)) {
            self.uncertain = true;
        }
        result
    }
    /// Trusted supervisor only: callers hold current admitted membership authority.
    /// This accepts remote roster members, excluding the actual local endpoint.
    pub fn sync_remote_members(
        &mut self,
        team: &str,
        revision: u64,
        members: &[RemoteMember],
    ) -> Result<(), EngineError> {
        if self.uncertain {
            return Err(EngineError::StorageUncertain);
        }
        label(team)?;
        let mut members = members.to_vec();
        members.sort_by_key(|m| m.peer);
        for member in &members {
            label(&member.person)?;
            label(&member.machine)?;
        }
        if members.windows(2).any(|pair| pair[0].peer == pair[1].peer) {
            return Err(EngineError::Conflict);
        }
        let prior = self.mail.remote_snapshot();
        let prior = self.remote_result(prior)?;
        let unchanged = prior.as_ref().is_some_and(|prior| {
            prior.team == team && prior.revision == revision && prior.members == members
        });
        if let Some(prior) = prior
            && prior.team == team
        {
            if prior.revision > revision {
                return Err(EngineError::StaleAttempt);
            }
            if prior.revision == revision && prior.members != members {
                return Err(EngineError::Conflict);
            }
        }
        if !unchanged {
            let result = self.mail.sync_remote_roster(team, revision, &members);
            self.remote_result(result)?;
        }
        self.remote_generations
            .retain(|peer, _| members.iter().any(|m| m.peer == *peer));
        self.remote_ready = true;
        Ok(())
    }
    /// Hide cached routes immediately after a trusted leave/disable boundary.
    pub fn suspend_remote_directory(&mut self) {
        self.remote_ready = false;
        for (_, active) in self.remote_generations.values_mut() {
            *active = false;
        }
    }
    pub fn remote_presence(
        &mut self,
        peer: EndpointId,
        generation: u64,
        presence: &wire::Presence,
    ) -> Result<(), EngineError> {
        if self.uncertain {
            return Err(EngineError::StorageUncertain);
        }
        if !self.remote_ready {
            return Err(EngineError::Unauthorized);
        }
        if self
            .remote_generations
            .get(&peer)
            .is_some_and(|(previous, active)| {
                *previous > generation || (*previous == generation && !active)
            })
        {
            return Err(EngineError::StaleAttempt);
        }
        let snapshot = self.mail.remote_snapshot();
        let snapshot = self
            .remote_result(snapshot)?
            .ok_or(EngineError::Unauthorized)?;
        let owner = snapshot
            .members
            .iter()
            .find(|m| m.peer == peer)
            .ok_or(EngineError::Unauthorized)?;
        let mut presence = presence.clone();
        let mut names = std::collections::BTreeSet::new();
        for session in &mut presence.sessions {
            label(&session.name)?;
            if !names.insert(session.name.clone()) {
                return Err(EngineError::Conflict);
            }
            // Attribute to authenticated roster, never to asserted peer identity.
            session.person = owner.person.clone();
            session.machine = owner.machine.clone();
            session.address = format!("{}/{}/{}", owner.person, owner.machine, session.name);
            session.is_me = false;
            for value in [&session.agent, &session.cwd, &session.status]
                .into_iter()
                .chain(
                    [&session.repo, &session.branch, &session.doing]
                        .into_iter()
                        .filter_map(|v| v.as_ref()),
                )
            {
                if value.len() > registry::MAX_TEXT_BYTES || value.chars().any(char::is_control) {
                    return Err(EngineError::InvalidInput);
                }
            }
        }
        for value in presence.known.iter_mut().chain(&mut presence.kept) {
            let name = known_name(value, owner)?;
            *value = format!("{}/{}/{name}", owner.person, owner.machine);
        }
        // The protected snapshot includes presence only for its current team.
        let unchanged = snapshot.presence.iter().any(|prior| {
            prior.peer == peer
                && prior.generation == generation
                && prior.online
                && prior.presence == presence
        });
        if !unchanged {
            let result =
                self.mail
                    .save_remote_presence(&snapshot.team, peer, generation, &presence);
            self.remote_result(result)?;
        }
        self.remote_generations.insert(peer, (generation, true));
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn remote_disconnected(
        &mut self,
        peer: EndpointId,
        generation: u64,
    ) -> Result<(), EngineError> {
        if self.uncertain {
            return Err(EngineError::StorageUncertain);
        }
        if self
            .remote_generations
            .get(&peer)
            .is_none_or(|(previous, _)| *previous <= generation)
        {
            self.remote_generations.insert(peer, (generation, false));
        }
        let result = self.mail.offline_remote(peer, generation);
        self.remote_result(result)?;
        let result = self.mail.disconnect_transport(peer, generation);
        self.remote_result(result)?;
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn resolve_incoming_recipient(&self, address: &str) -> Result<String, EngineError> {
        if self.uncertain {
            return Err(EngineError::StorageUncertain);
        }
        let prefix = format!("{}/{}/", self.identity.person, self.identity.machine);
        let name = address
            .strip_prefix(&prefix)
            .ok_or(EngineError::Unauthorized)?;
        label(name)?;
        let target = self
            .registry
            .registry()?
            .resolve(&registry::DirectoryFilter {
                person: Some(self.identity.person.clone()),
                machine: Some(self.identity.machine.clone()),
                name: Some(name.into()),
                ..Default::default()
            })?;
        Ok(mailbox_address(&target.conversation.into()))
    }
    pub fn local_presence(&self) -> Result<wire::Presence, EngineError> {
        if self.uncertain {
            return Err(EngineError::StorageUncertain);
        }
        let local = self
            .registry
            .registry()?
            .who(&registry::DirectoryFilter::default(), registry::MAX_RESULTS)?;
        let mut result = wire::Presence::default();
        for entry in local {
            let conversation: Conversation = entry.conversation.into();
            let unread = self
                .mail
                .unread(&mailbox_address(&conversation))?
                .try_into()
                .map_err(|_| EngineError::Limit)?;
            let address = address(&conversation);
            result.known.push(address.clone());
            if !entry.online {
                result.kept.push(address);
                continue;
            }
            let repository = entry.repository.map(|p| p.to_string_lossy().into_owned());
            result.sessions.push(wire::SessionPresence {
                address,
                name: conversation.name,
                person: conversation.person,
                machine: conversation.machine,
                agent: String::new(),
                cwd: repository.clone().unwrap_or_default(),
                repo: repository,
                branch: None,
                status: entry.status,
                since: None,
                doing: (!entry.doing.is_empty()).then_some(entry.doing),
                tier: wire::DeliveryTier::Pull,
                unread,
                is_me: false,
            });
        }
        Ok(result)
    }
}
