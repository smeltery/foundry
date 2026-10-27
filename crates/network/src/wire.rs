//! Wire representations only. Decoding signatures is not authorization.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum PeerFrame {
    Hello {
        team: String,
        proto: u32,
    },
    State {
        state: Box<StateDocument>,
    },
    Presence {
        presence: Presence,
    },
    Msg {
        msg: Box<Message>,
    },
    Ack {
        id: String,
        state: String,
        note: String,
    },
}

/// Bodies remain unverified structured data until a membership engine is present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedDocument {
    pub body: Value,
    pub by: String,
    pub sig: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateDocument {
    pub id: String,
    pub founder: String,
    pub meta: SignedDocument,
    #[serde(default)]
    pub admissions: BTreeMap<String, SignedDocument>,
    pub members: BTreeMap<String, SignedDocument>,
    pub invites: BTreeMap<String, SignedDocument>,
    #[serde(default)]
    pub link: Option<SignedDocument>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attestations: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Presence {
    pub sessions: Vec<SessionPresence>,
    pub known: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionPresence {
    pub address: String,
    pub name: String,
    pub person: String,
    pub machine: String,
    pub agent: String,
    pub cwd: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub status: String,
    pub since: Option<i64>,
    pub doing: Option<String>,
    pub tier: DeliveryTier,
    pub unread: u32,
    pub is_me: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryTier {
    Push,
    Turn,
    Pull,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub from_agent: Option<String>,
    pub from_repo: Option<String>,
    pub to: String,
    pub via: Option<String>,
    pub reply_to: Option<String>,
    pub body: String,
    pub wake: bool,
    #[serde(default)]
    pub notify: bool,
    pub sent_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<SignedDocument>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<TextAttachment>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextAttachment {
    pub name: String,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}
