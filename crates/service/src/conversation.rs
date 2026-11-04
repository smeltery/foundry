//! Conversation and message views. Messages list queued follow-up instructions only;
//! the conversation adds the opening prompt, assistant replies and settled results;
//! the event log alone carries diagnostic and tool activity.
use crate::{Result, Service, ServiceError};
use foundry_contracts::{AdapterEvent, Message, Outcome};
use foundry_store::{ConversationRecord, StoreError};
use serde::Serialize;

/// Position of the opening prompt; stored event positions start at one.
pub const PROMPT_POSITION: u64 = 0;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConversationEntry {
    pub position: u64,
    #[serde(flatten)]
    pub content: ConversationContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationContent {
    Prompt {
        text: String,
    },
    FollowUp {
        message_id: String,
        attempt_id: Option<String>,
        text: String,
    },
    Reply {
        event_id: String,
        attempt_id: String,
        revision: u64,
        text: String,
    },
    Result {
        event_id: String,
        attempt_id: String,
        outcome: Outcome,
        text: String,
    },
}

/// `end_cursor` is the last returned position, or the request cursor for an empty page.
/// Pass it back as `after` to continue; `None` starts at the opening prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConversationPage {
    pub entries: Vec<ConversationEntry>,
    pub end_cursor: Option<u64>,
    pub has_more: bool,
}

/// `end_cursor` is the last returned message sequence, or the request cursor when empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MessagePage {
    pub messages: Vec<Message>,
    pub end_cursor: Option<u64>,
    pub has_more: bool,
}

impl Service {
    pub fn conversation(
        &self,
        session_id: &str,
        after: Option<u64>,
        limit: u32,
    ) -> Result<ConversationPage> {
        let session = self.store.get(session_id)?;
        let mut entries = Vec::new();
        if after.is_none() {
            entries.push(ConversationEntry {
                position: PROMPT_POSITION,
                content: ConversationContent::Prompt {
                    text: session.prompt,
                },
            });
        }
        let records = self.store.conversation_events(
            session_id,
            after.unwrap_or(PROMPT_POSITION),
            lookahead(limit)?,
        )?;
        for record in records {
            entries.push(project(record)?);
        }
        let (has_more, end_cursor) = paginate(&mut entries, limit, after, |entry| entry.position);
        Ok(ConversationPage {
            entries,
            end_cursor,
            has_more,
        })
    }

    pub fn message_page(
        &self,
        session_id: &str,
        after: Option<u64>,
        limit: u32,
    ) -> Result<MessagePage> {
        let mut messages =
            self.store
                .messages(session_id, after.unwrap_or(0), lookahead(limit)?)?;
        let (has_more, end_cursor) =
            paginate(&mut messages, limit, after, |message| message.sequence);
        Ok(MessagePage {
            messages,
            end_cursor,
            has_more,
        })
    }
}

/// One extra row reveals whether another page exists.
fn lookahead(limit: u32) -> Result<u32> {
    limit
        .checked_add(1)
        .ok_or(ServiceError::Invalid("page limit exceeds supported range"))
}

fn paginate<T>(
    items: &mut Vec<T>,
    limit: u32,
    after: Option<u64>,
    position: impl Fn(&T) -> u64,
) -> (bool, Option<u64>) {
    let limit = limit as usize;
    let has_more = items.len() > limit;
    items.truncate(limit);
    (has_more, items.last().map(position).or(after))
}

fn project(record: ConversationRecord) -> Result<ConversationEntry> {
    let ConversationRecord {
        position,
        event,
        message,
    } = record;
    let content = if let Some(message) = message {
        ConversationContent::FollowUp {
            message_id: message.id,
            attempt_id: message.attempt_id,
            text: message.text,
        }
    } else {
        match serde_json::from_value(event.data).map_err(StoreError::Json)? {
            AdapterEvent::Text { text, .. } if event.kind == "text" => ConversationContent::Reply {
                event_id: event.id,
                attempt_id: event.attempt_id,
                revision: event.revision,
                text,
            },
            AdapterEvent::Settled {
                outcome, result, ..
            } if event.kind == "adapter_settled" => ConversationContent::Result {
                event_id: event.id,
                attempt_id: event.attempt_id,
                outcome,
                text: result,
            },
            _ => {
                return Err(ServiceError::Coordination(
                    "stored conversation event does not match its kind",
                ));
            }
        }
    };
    Ok(ConversationEntry { position, content })
}
