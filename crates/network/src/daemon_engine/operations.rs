use super::*;
use std::path::PathBuf;
impl Engine {
    pub(super) fn register(
        &mut self,
        connection: ConnectionId,
        name: String,
        repository: PathBuf,
        share_path: bool,
    ) -> Result<Outcome, EngineError> {
        let conversation = registry::Conversation {
            person: self.identity.person.clone(),
            machine: self.identity.machine.clone(),
            name,
        };
        let registered = self.registry.transact(|r| {
            let result = r.register(registry::Registration {
                conversation,
                repository,
                share_path,
            })?;
            r.attach(
                &result.conversation,
                &result.credential,
                connection,
                registry::DeliveryCapability::Pull,
            )?;
            Ok(result)
        })?;
        Ok(Outcome::Registered {
            auth: Auth {
                conversation: registered.conversation.into(),
                credential: *registered.credential.as_bytes(),
            },
        })
    }
    pub(super) fn attach(
        &mut self,
        connection: ConnectionId,
        auth: Auth,
    ) -> Result<Outcome, EngineError> {
        self.registry.transact(|r| {
            r.attach(
                &auth.conversation.native(),
                &auth.secret(),
                connection,
                registry::DeliveryCapability::Pull,
            )
        })?;
        Ok(Outcome::Attached)
    }
    pub(super) fn who(&self, filter: Filter) -> Result<Outcome, EngineError> {
        let entries = self
            .registry
            .registry()?
            .who(&filter.native(), registry::MAX_RESULTS)?;
        Ok(Outcome::Directory {
            entries: entries.into_iter().map(Entry::from).collect(),
        })
    }
    pub(super) fn send(
        &mut self,
        connection: ConnectionId,
        auth: Auth,
        mut message: mail::Message,
        to: Filter,
    ) -> Result<Outcome, EngineError> {
        self.authorize(connection, &auth)?;
        let recipient = self
            .registry
            .registry()?
            .resolve(&to.native())?
            .conversation;
        message.sender = mailbox_address(&auth.conversation);
        message.recipient = mailbox_address(&recipient.into());
        let duplicate =
            self.mail.insert(&message, mail::State::Pending)? == mail::Insert::Duplicate;
        let state = self.mail.get(&message.id)?.state.into();
        Ok(Outcome::Sent {
            message_id: message.id,
            duplicate,
            state,
        })
    }
}
