use super::*;
use crate::wire;
use iroh::EndpointId;
use serde::Serialize;

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct RemoteMember {
    pub peer: EndpointId,
    pub person: String,
    pub machine: String,
}
pub(crate) struct RemoteSnapshot {
    pub team: String,
    pub revision: u64,
    pub members: Vec<RemoteMember>,
    pub presence: Vec<RemotePresence>,
}
pub(crate) struct RemotePresence {
    pub peer: EndpointId,
    pub generation: u64,
    pub presence: wire::Presence,
    pub online: bool,
}
fn number(bytes: Vec<u8>) -> rusqlite::Result<u64> {
    Ok(u64::from_be_bytes(
        bytes
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
    ))
}
pub(super) fn encoded(value: &impl Serialize) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(|_| MailError::InvalidPayload)?;
    if bytes.len() > crate::codec::MAX_FRAME_BYTES {
        return Err(MailError::Capacity);
    }
    Ok(bytes)
}
impl MailStore {
    pub(crate) fn remote_snapshot(&self) -> Result<Option<RemoteSnapshot>> {
        self.protection.check()?;
        let invalid_members:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM remote_members WHERE typeof(peer)!='blob' OR length(peer)!=32 OR typeof(person)!='text' OR typeof(machine)!='text' OR length(CAST(person AS BLOB)) NOT BETWEEN 1 AND 512 OR length(CAST(machine AS BLOB)) NOT BETWEEN 1 AND 512) OR (SELECT coalesce(sum(length(peer)+length(CAST(person AS BLOB))+length(CAST(machine AS BLOB))+64),0) FROM remote_members)>?1",[crate::codec::MAX_FRAME_BYTES],|r|r.get(0))?;
        if invalid_members {
            return Err(MailError::InvalidPayload);
        }
        let epoch: Option<(String, Vec<u8>)> = self
            .connection
            .query_row(
                "SELECT team,revision FROM remote_epoch WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((team, revision)) = epoch else {
            let orphan:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM remote_members) OR EXISTS(SELECT 1 FROM remote_presence)",[],|r|r.get(0))?;
            return if orphan {
                Err(MailError::InvalidPayload)
            } else {
                Ok(None)
            };
        };
        let invalid:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM remote_presence WHERE length(payload)>?1 OR typeof(payload)!='blob') OR (SELECT coalesce(sum(length(payload)),0) FROM remote_presence)>?2",params![crate::codec::MAX_FRAME_BYTES,MAX_BYTES],|r|r.get(0))?;
        if invalid {
            return Err(MailError::InvalidPayload);
        }
        let mut query = self
            .connection
            .prepare("SELECT peer,person,machine FROM remote_members ORDER BY peer")?;
        let members = query
            .query_map([], |r| {
                let peer: Vec<u8> = r.get(0)?;
                let peer: [u8; 32] = peer.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?;
                Ok(RemoteMember {
                    peer: EndpointId::from_bytes(&peer)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    person: r.get(1)?,
                    machine: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        encoded(&members)?;
        let mut query=self.connection.prepare("SELECT p.peer,p.generation,p.payload,p.online FROM remote_presence p JOIN remote_members m ON m.peer=p.peer WHERE p.team=?1 ORDER BY p.peer")?;
        let presence = query
            .query_map([&team], |r| {
                let peer: Vec<u8> = r.get(0)?;
                let peer: [u8; 32] = peer.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?;
                let raw: Vec<u8> = r.get(2)?;
                Ok(RemotePresence {
                    peer: EndpointId::from_bytes(&peer)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    generation: number(r.get(1)?)?,
                    presence: serde_json::from_slice(&raw)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    online: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(RemoteSnapshot {
            team,
            revision: number(revision)?,
            members,
            presence,
        }))
    }
    pub(crate) fn sync_remote_roster(
        &mut self,
        team: &str,
        revision: u64,
        members: &[RemoteMember],
    ) -> Result<()> {
        identifier(team)?;
        encoded(&members)?;
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO remote_epoch VALUES(1,?1,?2) ON CONFLICT(id) DO UPDATE SET team=excluded.team,revision=excluded.revision",params![team,revision.to_be_bytes()])?;
        tx.execute("DELETE FROM remote_members", [])?;
        for member in members {
            tx.execute(
                "INSERT INTO remote_members VALUES(?1,?2,?3)",
                params![member.peer.as_bytes(), member.person, member.machine],
            )?;
        }
        tx.execute("DELETE FROM remote_presence WHERE team!=?1 OR peer NOT IN(SELECT peer FROM remote_members)",[team])?;
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(())
    }
    pub(crate) fn save_remote_presence(
        &mut self,
        team: &str,
        peer: EndpointId,
        generation: u64,
        presence: &wire::Presence,
    ) -> Result<()> {
        let bytes = encoded(presence)?;
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (total,prior):(i64,i64)=tx.query_row("SELECT coalesce(sum(length(payload)),0),coalesce(max(CASE WHEN peer=?1 THEN length(payload) ELSE 0 END),0) FROM remote_presence",[peer.as_bytes()],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if total - prior + bytes.len() as i64 > MAX_BYTES {
            return Err(MailError::Capacity);
        }
        tx.execute("INSERT INTO remote_presence VALUES(?1,?2,?3,?4,1) ON CONFLICT(peer) DO UPDATE SET team=excluded.team,generation=excluded.generation,payload=excluded.payload,online=1",params![peer.as_bytes(),team,generation.to_be_bytes(),bytes])?;
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(())
    }
    pub(crate) fn offline_remote(&mut self, peer: EndpointId, generation: u64) -> Result<()> {
        self.protection.check()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE remote_presence SET online=0 WHERE peer=?1 AND generation=?2",
            params![peer.as_bytes(), generation.to_be_bytes()],
        )?;
        self.protection.check()?;
        tx.commit()?;
        self.protection.sync()?;
        Ok(())
    }
}
