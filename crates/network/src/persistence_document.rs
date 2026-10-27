use super::*;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Document {
    pub profile: WireProfile,
    pub snapshot: MembershipSnapshot,
}
impl Document {
    pub fn matches(&self, team: &str, founder: EndpointId, profile: &WireProfile) -> Result<()> {
        if self.snapshot.team != team
            || self.snapshot.founder != founder
            || serde_json::to_value(&self.profile).map_err(|_| PersistenceError::Invalid)?
                != serde_json::to_value(profile).map_err(|_| PersistenceError::Invalid)?
        {
            return Err(PersistenceError::IdentityMismatch);
        }
        Ok(())
    }
}
pub(super) fn encode(snapshot: &MembershipSnapshot, profile: &WireProfile) -> Result<Vec<u8>> {
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > MAX_MEMBERSHIP_BYTES - self.0.len() {
                return Err(io::Error::other("payload limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[derive(Serialize)]
    struct Borrowed<'a> {
        profile: &'a WireProfile,
        snapshot: &'a MembershipSnapshot,
    }
    let mut sink = Bounded(Vec::new());
    serde_json::to_writer(&mut sink, &Borrowed { profile, snapshot })
        .map_err(|_| PersistenceError::TooLarge)?;
    Ok(sink.0)
}
pub(super) fn validate(
    snapshot: &MembershipSnapshot,
    profile: &WireProfile,
    now: i64,
) -> Result<MembershipState> {
    let (state, report) =
        MembershipState::bootstrap(&snapshot.team, snapshot.founder, snapshot, profile, now)
            .map_err(|_| PersistenceError::Corrupt)?;
    if report.rejected != 0 || report.proof_required != 0 || state.snapshot() != snapshot {
        return Err(PersistenceError::Corrupt);
    }
    Ok(state)
}
pub(super) fn read(connection: &Connection) -> Result<Option<(u64, Document)>> {
    let count: i64 =
        connection.query_row("SELECT count(*) FROM projection", [], |row| row.get(0))?;
    if count > 1 {
        return Err(PersistenceError::Corrupt);
    }
    let metadata: Option<(i64, i64)> = connection
        .query_row(
            "SELECT revision,length(CAST(payload AS BLOB)) FROM projection WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((revision, length)) = metadata else {
        return if count == 0 {
            Ok(None)
        } else {
            Err(PersistenceError::Corrupt)
        };
    };
    if revision <= 0 {
        return Err(PersistenceError::Corrupt);
    }
    if length < 0 || length as u64 > MAX_MEMBERSHIP_BYTES as u64 {
        return Err(PersistenceError::TooLarge);
    }
    let bytes: Vec<u8> = connection.query_row(
        "SELECT CAST(payload AS BLOB) FROM projection WHERE id=1",
        [],
        |row| row.get(0),
    )?;
    let document = serde_json::from_slice(&bytes).map_err(|_| PersistenceError::Corrupt)?;
    Ok(Some((revision as u64, document)))
}
