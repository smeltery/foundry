use super::*;
impl Conversation {
    pub(super) fn native(&self) -> registry::Conversation {
        registry::Conversation {
            person: self.person.clone(),
            machine: self.machine.clone(),
            name: self.name.clone(),
        }
    }
}
impl From<registry::Conversation> for Conversation {
    fn from(c: registry::Conversation) -> Self {
        Self {
            person: c.person,
            machine: c.machine,
            name: c.name,
        }
    }
}
impl Auth {
    pub(super) fn secret(&self) -> registry::SessionCredential {
        registry::SessionCredential::from_bytes(self.credential)
    }
}
impl Filter {
    pub(super) fn native(self) -> registry::DirectoryFilter {
        registry::DirectoryFilter {
            person: self.person,
            machine: self.machine,
            name: self.name,
            repository: self.repository,
            online_only: self.online_only,
        }
    }
}
impl From<registry::RegistryError> for EngineError {
    fn from(error: registry::RegistryError) -> Self {
        use registry::RegistryError::*;
        match error {
            Unauthorized => Self::Unauthorized,
            NotFound => Self::NotFound,
            Ambiguous => Self::Ambiguous,
            Excluded => Self::Excluded,
            Limit => Self::Limit,
            Entropy => Self::Entropy,
            InvalidPath | InvalidText => Self::InvalidInput,
        }
    }
}
impl From<registry::DurableError> for EngineError {
    fn from(error: registry::DurableError) -> Self {
        match error {
            registry::DurableError::Registry(error) => error.into(),
            registry::DurableError::Schema | registry::DurableError::Corrupt => Self::Schema,
            _ => Self::StorageUncertain,
        }
    }
}
impl From<mail::MailError> for EngineError {
    fn from(error: mail::MailError) -> Self {
        match error {
            mail::MailError::InvalidInput => Self::InvalidInput,
            mail::MailError::InvalidPayload => Self::InvalidPayload,
            mail::MailError::UnsupportedPayload => Self::UnsupportedPayload,
            mail::MailError::Conflict => Self::Conflict,
            mail::MailError::Capacity => Self::Limit,
            mail::MailError::Missing => Self::NotFound,
            mail::MailError::WrongRecipient => Self::Unauthorized,
            mail::MailError::StaleAttempt => Self::StaleAttempt,
            mail::MailError::InvalidTransition => Self::InvalidTransition,
            mail::MailError::Output => Self::Output,
            mail::MailError::Schema => Self::Schema,
            mail::MailError::Storage(_) | mail::MailError::Protection(_) => Self::StorageUncertain,
        }
    }
}
impl From<mail::State> for MailState {
    fn from(state: mail::State) -> Self {
        match state {
            mail::State::Pending => Self::Pending,
            mail::State::Held => Self::Held,
            mail::State::Rejected => Self::Rejected,
            mail::State::Delivered => Self::Delivered,
            mail::State::Consumed => Self::Consumed,
        }
    }
}
impl From<registry::DirectoryEntry> for Entry {
    fn from(entry: registry::DirectoryEntry) -> Self {
        Self {
            conversation: entry.conversation.into(),
            repository: entry.repository,
            online: entry.online,
            status: entry.status,
            doing: entry.doing,
            capabilities: entry
                .capabilities
                .into_iter()
                .map(|c| match c {
                    registry::DeliveryCapability::Pull => Capability::Pull,
                    registry::DeliveryCapability::Hook => Capability::Hook,
                    registry::DeliveryCapability::Push => Capability::Push,
                })
                .collect(),
        }
    }
}
impl Mail {
    pub(super) fn from_message(
        message: &mail::Message,
        attempt: Option<&mail::Attempt>,
    ) -> mail::Result<Self> {
        let display = message.display_body()?;
        Ok(Self {
            message_id: message.id.clone(),
            sender: message.sender.clone(),
            recipient: message.recipient.clone(),
            body_base64: data_encoding::BASE64.encode(&display),
            reply_to: message.reply_to.clone(),
            wake: message.intent.wake,
            notify: message.intent.notify,
            state: if attempt.is_some() {
                MailState::Pending
            } else {
                MailState::Consumed
            },
            attempt_id: attempt.map(mail::Attempt::id),
            expires_ms: attempt.and_then(mail::Attempt::deadline_ms),
        })
    }
}
