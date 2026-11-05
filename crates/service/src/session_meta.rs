//! Archive, rename, tags and delete as reported to callers
//! (`docs/architecture/session-api.md`). None of these changes lifecycle,
//! outcome or attempt history; the store serializes concurrent writers. Wire
//! names are provisional pending the naming decision.
use crate::{Result, Service};
use foundry_contracts::Session;
use foundry_store::DERIVED_TITLE_CHARS;
pub use foundry_store::DeleteReceipt;

/// How a delete request treats the session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DeleteMode {
    /// Tombstone: hidden from reads and lists, content dropped, audit and
    /// digest-bearing records kept.
    #[default]
    Tombstone,
    /// Keep the session and its history; archive it instead.
    Preserve,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeleteOutcome {
    Archived(Session),
    Deleted(DeleteReceipt),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Renamed {
    pub session: Session,
    /// The stored title, or the derived one when none is stored.
    pub title: String,
}

/// The stored title wins; otherwise the first 200 characters of the prompt.
pub fn display_title(session: &Session) -> String {
    session
        .metadata
        .title
        .clone()
        .unwrap_or_else(|| session.prompt.chars().take(DERIVED_TITLE_CHARS).collect())
}

impl Service {
    /// Removes the session from the default list. Repeatable.
    pub fn archive(&mut self, id: &str) -> Result<Session> {
        Ok(self.store.set_archived(id, true)?)
    }

    /// Restores the session to the default list without touching history. Repeatable.
    pub fn unarchive(&mut self, id: &str) -> Result<Session> {
        Ok(self.store.set_archived(id, false)?)
    }

    /// An empty or absent title restores the derived one.
    pub fn rename(&mut self, id: &str, title: Option<&str>) -> Result<Renamed> {
        let session = self.store.rename_session(id, title)?;
        Ok(Renamed {
            title: display_title(&session),
            session,
        })
    }

    pub fn tags(&self, id: &str) -> Result<Vec<String>> {
        Ok(self.store.session_tags(id)?)
    }

    pub fn append_tags(&mut self, id: &str, tags: &[String]) -> Result<Vec<String>> {
        Ok(self.store.append_tags(id, tags)?)
    }

    /// Both provisional replace wire forms reach this; neither is a partial edit.
    pub fn replace_tags(&mut self, id: &str, tags: &[String]) -> Result<Vec<String>> {
        Ok(self.store.replace_tags(id, tags)?)
    }

    /// A tombstone of a session with a live attempt or restore owner is refused
    /// with `ContractError::Conflict`. Preserve only archives, which never
    /// affects execution, so it is accepted while running.
    pub fn delete(&mut self, id: &str, mode: DeleteMode) -> Result<DeleteOutcome> {
        Ok(match mode {
            DeleteMode::Tombstone => DeleteOutcome::Deleted(self.store.delete_session(id)?),
            DeleteMode::Preserve => DeleteOutcome::Archived(self.store.set_archived(id, true)?),
        })
    }
}
