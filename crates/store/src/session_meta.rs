//! Session archive, title, tags and tombstone delete (`docs/architecture/session-api.md`,
//! "Archive, unarchive and delete" and "Title, tags and project grouping").
//!
//! Metadata is not execution state: these writes never change lifecycle,
//! outcome, attempts or `Session::version`, so an admission or restore that
//! expects a version is unaffected. Every write runs in one IMMEDIATE
//! transaction that reads and rewrites the current rows, so concurrent writers
//! from any connection serialize and none of their changes is lost.
use crate::{Result, Store, admission, conflict, now_ms, read_session};
use foundry_contracts::{
    ContractError, Lifecycle, Session,
    session_v2::{MAX_LABEL_BYTES, MAX_TAGS, MAX_TITLE_CHARS},
};
use rusqlite::{Connection, TransactionBehavior, params};

/// A committed tombstone. The session row stays as an audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteReceipt {
    pub session_id: String,
    pub deleted_ms: u64,
}

impl Store {
    /// Archive state only; repeating the call reports the same session.
    pub fn set_archived(&mut self, session_id: &str, archived: bool) -> Result<Session> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut session = read_session(&tx, session_id)?;
        session.metadata.archived = archived;
        tx.execute(
            "UPDATE sessions SET archived=?1,body=?2 WHERE id=?3",
            params![archived, serde_json::to_string(&session)?, session_id],
        )?;
        tx.commit()?;
        Ok(session)
    }

    /// `None` or an empty title clears the stored title, so readers derive it
    /// from the prompt again.
    pub fn rename_session(&mut self, session_id: &str, title: Option<&str>) -> Result<Session> {
        let title = title.filter(|title| !title.is_empty());
        if let Some(title) = title
            && (title.trim().is_empty() || title.chars().count() > MAX_TITLE_CHARS)
        {
            return Err(invalid("title must contain 1 to 200 characters"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut session = read_session(&tx, session_id)?;
        session.metadata.title = title.map(str::to_owned);
        tx.execute(
            "UPDATE sessions SET title=?1,body=?2 WHERE id=?3",
            params![title, serde_json::to_string(&session)?, session_id],
        )?;
        tx.commit()?;
        Ok(session)
    }

    /// Current tags in stored order; empty for an untagged session.
    pub fn session_tags(&self, session_id: &str) -> Result<Vec<String>> {
        read_session(&self.connection, session_id)?;
        stored_tags(&self.connection, session_id)
    }

    /// Set union: existing tags keep their order and new ones follow in request
    /// order, without duplicates. Returns the stored list.
    pub fn append_tags(&mut self, session_id: &str, tags: &[String]) -> Result<Vec<String>> {
        self.write_tags(session_id, tags, true)
    }

    /// Sets the whole list; an empty request clears it. Duplicates in the
    /// request keep their first position. Returns the stored list.
    pub fn replace_tags(&mut self, session_id: &str, tags: &[String]) -> Result<Vec<String>> {
        self.write_tags(session_id, tags, false)
    }

    /// Tombstone delete. A session with a live attempt or restore owner is
    /// refused. Otherwise the session disappears from every read and list, and
    /// its content is dropped in the same transaction: prompt, result, title,
    /// creation request, tags, message text and event data. Identity,
    /// lifecycle, outcome, lineage and every digest-bearing row (tool receipts,
    /// checkpoints, witnesses, continuation and completion records) are kept,
    /// so integrity checks over them stay verifiable.
    pub fn delete_session(&mut self, session_id: &str) -> Result<DeleteReceipt> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut session = read_session(&tx, session_id)?;
        if session.lifecycle == Lifecycle::Running || session.active_attempt_id.is_some() {
            return Err(conflict("a running session cannot be deleted"));
        }
        admission::exclusive(&tx, session_id, None)?;
        let deleted_ms = now_ms()?;
        session.prompt.clear();
        session.result = None;
        session.metadata.title = None;
        tx.execute(
            "UPDATE sessions SET body=?1,creation='{}',title=NULL,deleted_ms=?2 WHERE id=?3",
            params![serde_json::to_string(&session)?, deleted_ms, session_id],
        )?;
        tx.execute("DELETE FROM session_tags WHERE session_id=?1", [session_id])?;
        tx.execute(
            "UPDATE messages SET text='' WHERE session_id=?1",
            [session_id],
        )?;
        tx.execute(
            "UPDATE events SET data='null' WHERE session_id=?1",
            [session_id],
        )?;
        tx.commit()?;
        Ok(DeleteReceipt {
            session_id: session_id.into(),
            deleted_ms: deleted_ms as u64,
        })
    }

    fn write_tags(
        &mut self,
        session_id: &str,
        tags: &[String],
        append: bool,
    ) -> Result<Vec<String>> {
        for tag in tags {
            if tag.trim().is_empty() || tag.len() > MAX_LABEL_BYTES {
                return Err(invalid("tag must contain 1 to 64 UTF-8 bytes"));
            }
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        read_session(&tx, session_id)?;
        let mut next = if append {
            stored_tags(&tx, session_id)?
        } else {
            Vec::new()
        };
        for tag in tags {
            if !next.contains(tag) {
                next.push(tag.clone());
            }
        }
        if next.len() > MAX_TAGS {
            return Err(invalid("too many tags"));
        }
        tx.execute("DELETE FROM session_tags WHERE session_id=?1", [session_id])?;
        for (position, tag) in next.iter().enumerate() {
            tx.execute(
                "INSERT INTO session_tags(session_id,tag,position) VALUES(?1,?2,?3)",
                params![session_id, tag, position as i64],
            )?;
        }
        tx.commit()?;
        Ok(next)
    }
}

fn stored_tags(connection: &Connection, session_id: &str) -> Result<Vec<String>> {
    let mut statement =
        connection.prepare("SELECT tag FROM session_tags WHERE session_id=?1 ORDER BY position")?;
    let tags = statement
        .query_map([session_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(tags)
}

fn invalid(message: &str) -> crate::StoreError {
    ContractError::Invalid(message.into()).into()
}
