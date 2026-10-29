use super::*;
/// Explicit storage format. Text never attempts JSON parsing, even when its
/// bytes resemble an envelope. The entire wire JSON counts toward mailbox quota.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PayloadEncoding {
    #[default]
    Text,
    WireJson,
}
impl PayloadEncoding {
    pub(super) fn code(self) -> i64 {
        match self {
            Self::Text => 0,
            Self::WireJson => 1,
        }
    }
    pub(super) fn decode(value: i64) -> rusqlite::Result<Self> {
        match value {
            0 => Ok(Self::Text),
            1 => Ok(Self::WireJson),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}
impl Message {
    fn wire_payload(&self) -> Result<crate::wire::Message> {
        if self.body.len() > 256 * 1024 {
            return Err(MailError::InvalidPayload);
        }
        let wire: crate::wire::Message =
            serde_json::from_slice(&self.body).map_err(|_| MailError::InvalidPayload)?;
        if wire.id != self.id
            || wire.from != self.sender
            || wire.reply_to != self.reply_to
            || wire.wake != self.intent.wake
            || wire.notify != self.intent.notify
            || wire.to.is_empty()
            || wire.to.len() > 256
            || wire.sent_at < 0
        {
            return Err(MailError::InvalidPayload);
        }
        if !wire.files.is_empty() || wire.approval.is_some() {
            return Err(MailError::UnsupportedPayload);
        }
        Ok(wire)
    }
    /// Pure bounded decoding before any inbox read-state mutation. Authentication
    /// and recipient resolution remain the receiving peer/registry's responsibility.
    pub(super) fn validate_payload(&self) -> Result<()> {
        if self.encoding == PayloadEncoding::WireJson {
            self.wire_payload()?;
        }
        Ok(())
    }
    /// Privileged display sink. The immutable stored envelope is never replaced
    /// with this extracted text, so timestamp and metadata conflicts remain visible.
    pub fn display_body(&self) -> Result<Vec<u8>> {
        match self.encoding {
            PayloadEncoding::Text => Ok(self.body.clone()),
            PayloadEncoding::WireJson => Ok(self.wire_payload()?.body.into_bytes()),
        }
    }
}
