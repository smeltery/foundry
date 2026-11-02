use super::*;
use crate::wire;
use iroh::EndpointId;

pub const TRANSPORT_RETENTION_MS: i64 = 7 * 86_400_000;
#[derive(Clone, PartialEq, Eq)]
pub enum TransportDisposition {
    Queued,
    Delivered,
    Held,
    Refused,
    Retry,
    Notified,
    NotNotified,
    Other(String),
}
impl TransportDisposition {
    pub fn from_state(state: &str) -> Result<Self> {
        if state.len() > crate::codec::MAX_FRAME_BYTES {
            return Err(MailError::InvalidInput);
        }
        Ok(match state {
            "queued" => Self::Queued,
            "delivered" => Self::Delivered,
            "held" => Self::Held,
            "refused" => Self::Refused,
            "retry" => Self::Retry,
            "notified" => Self::Notified,
            "not notified" => Self::NotNotified,
            other => Self::Other(other.into()),
        })
    }
    pub fn as_state(&self) -> &str {
        match self {
            Self::Queued => "queued",
            Self::Delivered => "delivered",
            Self::Held => "held",
            Self::Refused => "refused",
            Self::Retry => "retry",
            Self::Notified => "notified",
            Self::NotNotified => "not notified",
            Self::Other(state) => state,
        }
    }
    pub fn taken(&self) -> bool {
        self.as_state() != "retry" && self.as_state() != "refused"
    }
}

#[derive(Clone)]
pub struct TransportMessage {
    pub peer: EndpointId,
    pub team: String,
    pub revision: u64,
    pub message: wire::Message,
}
pub struct TransportAttempt {
    queued: TransportMessage,
    payload: Vec<u8>,
    generation: u64,
    serial: i64,
}
impl TransportAttempt {
    pub fn message(&self) -> &TransportMessage {
        &self.queued
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
}
pub(super) fn serial(tx: &rusqlite::Transaction<'_>) -> Result<i64> {
    let previous: i64 = tx.query_row("SELECT value FROM sequence WHERE id=1", [], |r| r.get(0))?;
    let next = previous.checked_add(1).ok_or(MailError::Capacity)?;
    tx.execute("UPDATE sequence SET value=?1 WHERE id=1", [next])?;
    Ok(next)
}
fn payload(message: &wire::Message) -> Result<Vec<u8>> {
    identifier(&message.id)?;
    identifier(&message.from)?;
    identifier(&message.to)?;
    if let Some(reply) = &message.reply_to {
        identifier(reply)?;
    }
    timestamp(message.sent_at)?;
    let mut bytes = Payload(Vec::new());
    // The queue retains the complete original envelope. Wire admission remains
    // the peer profile's responsibility; never strip unimplemented metadata.
    serde_json::to_writer(&mut bytes, message).map_err(|_| MailError::InvalidPayload)?;
    Ok(bytes.0)
}
fn decode(row: &rusqlite::Row<'_>) -> rusqlite::Result<TransportMessage> {
    let peer: Vec<u8> = row.get(1)?;
    let peer: [u8; 32] = peer.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?;
    let raw: Vec<u8> = row.get(2)?;
    let message: wire::Message =
        serde_json::from_slice(&raw).map_err(|_| rusqlite::Error::InvalidQuery)?;
    if serde_json::to_vec(&message).map_err(|_| rusqlite::Error::InvalidQuery)? != raw {
        return Err(rusqlite::Error::InvalidQuery);
    }
    if message.id != row.get::<_, String>(3)? || message.sent_at != row.get::<_, i64>(4)? {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(TransportMessage {
        team: row.get(0)?,
        peer: EndpointId::from_bytes(&peer).map_err(|_| rusqlite::Error::InvalidQuery)?,
        message,
        revision: row.get(5)?,
    })
}
impl MailStore {
    /// Global-ID replacement follows queue semantics, independently of immutable
    /// inbox IDs. A replacement always invalidates every earlier attempt.
    pub fn enqueue_transport(
        &mut self,
        team: &str,
        peer: EndpointId,
        message: &wire::Message,
    ) -> Result<TransportMessage> {
        identifier(team)?;
        let bytes = payload(message)?;
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_queue(&tx)?;
        let (count,total,prior): (i64,i64,i64) = tx.query_row("SELECT count(*),coalesce(sum(length(payload)),0),coalesce(max(CASE WHEN id=?1 THEN length(payload) ELSE 0 END),0) FROM transport_outbox", [&message.id], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM transport_outbox WHERE id=?1)",
            [&message.id],
            |r| r.get(0),
        )?;
        if (count >= MAX_ROWS && !exists) || total - prior + bytes.len() as i64 > MAX_BYTES {
            return Err(MailError::Capacity);
        }
        let revision = serial(&tx)?;
        // Explicit delete/insert gives same-timestamp replacement a new order,
        // even if SQLite would otherwise recycle the last physical rowid.
        tx.execute("DELETE FROM transport_outbox WHERE id=?1", [&message.id])?;
        tx.execute("INSERT INTO transport_outbox(id,team,peer,payload,created_ms,revision) VALUES(?1,?2,?3,?4,?5,?6)",params![message.id,team,peer.as_bytes(),bytes,message.sent_at,revision])?;
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(TransportMessage {
            team: team.into(),
            peer,
            revision: revision as u64,
            message: message.clone(),
        })
    }
    /// Lookup is global by source ID, so a supervisor can detect replacement
    /// onto a different endpoint before retiring the old driver's ownership.
    pub fn transport_message(&self, id: &str) -> Result<Option<TransportMessage>> {
        identifier(id)?;
        self.protection.check()?;
        let length: Option<usize> = self
            .connection
            .query_row(
                "SELECT length(payload) FROM transport_outbox WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        if length.is_some_and(|n| n > crate::codec::MAX_FRAME_BYTES) {
            return Err(MailError::InvalidPayload);
        }
        self.connection
            .query_row(
                "SELECT team,peer,payload,id,created_ms,revision FROM transport_outbox WHERE id=?1",
                [id],
                decode,
            )
            .optional()
            .map_err(MailError::from)
    }
    /// Expiry applies globally, strictly before seven days; equality survives.
    /// Replay order follows enqueue/replacement order for equal timestamps.
    pub fn transport_for(
        &mut self,
        team: &str,
        peer: EndpointId,
        now_ms: i64,
    ) -> Result<Vec<TransportMessage>> {
        identifier(team)?;
        timestamp(now_ms)?;
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_queue(&tx)?;
        tx.execute(
            "DELETE FROM transport_outbox WHERE created_ms<?1",
            [now_ms.saturating_sub(TRANSPORT_RETENTION_MS)],
        )?;
        let messages = {
            let mut query=tx.prepare("SELECT team,peer,payload,id,created_ms,revision FROM transport_outbox WHERE team=?1 AND peer=?2 ORDER BY created_ms,revision")?;
            query
                .query_map(params![team, peer.as_bytes()], decode)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for queued in &messages {
            if payload(&queued.message)?.len() > MAX_BYTES as usize {
                return Err(MailError::InvalidPayload);
            }
        }
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(messages)
    }
    pub fn begin_transport_attempt(
        &mut self,
        queued: &TransportMessage,
        generation: u64,
    ) -> Result<TransportAttempt> {
        let bytes = payload(&queued.message)?;
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<Option<Vec<u8>>> = tx.query_row("SELECT generation FROM transport_outbox WHERE id=?1 AND team=?2 AND peer=?3 AND revision=?4 AND payload=?5",params![queued.message.id,queued.team,queued.peer.as_bytes(),queued.revision,bytes],|r|r.get(0)).optional()?;
        let Some(previous) = previous else {
            return Err(MailError::StaleAttempt);
        };
        if let Some(previous) = previous {
            let previous: [u8; 8] = previous.try_into().map_err(|_| MailError::InvalidPayload)?;
            if u64::from_be_bytes(previous) > generation {
                return Err(MailError::StaleAttempt);
            }
        }
        let serial = serial(&tx)?;
        let changed=tx.execute("UPDATE transport_outbox SET attempt=?1,generation=?2 WHERE id=?3 AND team=?4 AND peer=?5 AND revision=?6 AND payload=?7",params![serial,generation.to_be_bytes(),queued.message.id,queued.team,queued.peer.as_bytes(),queued.revision,bytes])?;
        if changed != 1 {
            return Err(MailError::StaleAttempt);
        }
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(TransportAttempt {
            queued: queued.clone(),
            payload: bytes,
            generation,
            serial,
        })
    }
    pub fn finish_transport_attempt(
        &mut self,
        attempt: &TransportAttempt,
        peer: EndpointId,
        generation: u64,
        disposition: TransportDisposition,
    ) -> Result<()> {
        if disposition.as_state().len() > crate::codec::MAX_FRAME_BYTES {
            return Err(MailError::InvalidInput);
        }
        if peer != attempt.queued.peer || generation != attempt.generation {
            return Err(MailError::StaleAttempt);
        }
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let condition = "id=?1 AND team=?2 AND peer=?3 AND revision=?4 AND payload=?5 AND attempt=?6 AND generation=?7";
        let sql = if disposition.as_state() == "retry" {
            format!("UPDATE transport_outbox SET attempt=NULL,generation=NULL WHERE {condition}")
        } else {
            format!("DELETE FROM transport_outbox WHERE {condition}")
        };
        if tx.execute(
            &sql,
            params![
                attempt.queued.message.id,
                attempt.queued.team,
                peer.as_bytes(),
                attempt.queued.revision,
                attempt.payload,
                attempt.serial,
                generation.to_be_bytes()
            ],
        )? != 1
        {
            return Err(MailError::StaleAttempt);
        }
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(())
    }
    pub fn disconnect_transport(&mut self, peer: EndpointId, generation: u64) -> Result<()> {
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("UPDATE transport_outbox SET attempt=NULL,generation=NULL WHERE peer=?1 AND generation=?2",params![peer.as_bytes(),generation.to_be_bytes()])?;
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(())
    }
    /// Call once under the actual single-instance lease, never per connection.
    pub fn recover_transport(&mut self) -> Result<()> {
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE transport_outbox SET attempt=NULL,generation=NULL",
            [],
        )?;
        tx.execute("UPDATE remote_presence SET online=0", [])?;
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(())
    }
}

struct Payload(Vec<u8>);
impl std::io::Write for Payload {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > crate::codec::MAX_FRAME_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other(
                "transport payload exceeds frame budget",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn validate_queue(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let (count,bytes,invalid):(i64,i64,bool)=tx.query_row("SELECT count(*),coalesce(sum(length(payload)),0),coalesce(max(typeof(payload)!='blob' OR length(payload)>?1 OR typeof(peer)!='blob' OR length(peer)!=32 OR length(CAST(id AS BLOB)) NOT BETWEEN 1 AND ?2 OR length(CAST(team AS BLOB)) NOT BETWEEN 1 AND ?2),0) FROM transport_outbox",params![crate::codec::MAX_FRAME_BYTES,MAX_MAIL_IDENTIFIER],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if count > MAX_ROWS || bytes > MAX_BYTES || invalid {
        return Err(MailError::InvalidPayload);
    }
    Ok(())
}
