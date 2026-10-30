use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Conversation {
    pub person: String,
    pub machine: String,
    pub name: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    pub conversation: Conversation,
    pub credential: [u8; 32],
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Filter {
    pub person: Option<String>,
    pub machine: Option<String>,
    pub name: Option<String>,
    pub repository: Option<PathBuf>,
    pub online_only: bool,
}
#[derive(Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Register {
        name: String,
        repository: PathBuf,
        #[serde(default)]
        share_path: bool,
    },
    Attach {
        auth: Auth,
    },
    Who {
        #[serde(default)]
        filter: Filter,
    },
    SetStatus {
        auth: Auth,
        status: String,
        #[serde(default)]
        doing: String,
    },
    Linked {},
    Send {
        auth: Auth,
        message_id: String,
        to: Filter,
        body: String,
        #[serde(default)]
        reply_to: Option<String>,
        #[serde(default = "yes")]
        wake: bool,
        #[serde(default)]
        notify: bool,
    },
    Inbox {
        auth: Auth,
        #[serde(default)]
        wait_secs: u64,
        #[serde(default)]
        peek: bool,
    },
    Unread {
        auth: Auth,
    },
    StopWaiting {
        auth: Auth,
    },
    Delivered {
        auth: Auth,
        attempt_id: i64,
    },
    PushFailed {
        auth: Auth,
        attempt_id: i64,
    },
    Shutdown {
        auth: Auth,
    },
}
fn yes() -> bool {
    true
}
#[derive(Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    pub result: ReplyResult,
}
impl Reply {
    pub fn should_shutdown(&self) -> bool {
        matches!(&self.result, ReplyResult::Ok(Outcome::Shutdown))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub enum ReplyResult {
    Ok(Outcome),
    Error(EngineError),
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Registered {
        auth: Auth,
    },
    Attached,
    Directory {
        entries: Vec<Entry>,
    },
    StatusUpdated,
    Linked {
        conversation: Conversation,
    },
    Sent {
        message_id: String,
        duplicate: bool,
        state: MailState,
    },
    TransportQueued {
        message_id: String,
        peer: String,
        revision: u64,
    },
    Inbox {
        messages: Vec<Mail>,
        text: String,
    },
    Unread {
        count: u64,
    },
    Delivered,
    PushFailed,
    WaitingStopped,
    Shutdown,
}
#[derive(Serialize, Deserialize)]
pub struct Entry {
    pub conversation: Conversation,
    pub repository: Option<PathBuf>,
    pub online: bool,
    pub status: String,
    pub doing: String,
    pub capabilities: Vec<Capability>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Pull,
    Hook,
    Push,
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MailState {
    Pending,
    Held,
    Rejected,
    Delivered,
    Consumed,
}
/// Display bytes use base64; complete inbox replies use the store-derived budget.
#[derive(Serialize, Deserialize)]
pub struct Mail {
    pub message_id: String,
    pub sender: String,
    pub recipient: String,
    pub body_base64: String,
    pub reply_to: Option<String>,
    pub wake: bool,
    pub notify: bool,
    pub state: MailState,
    pub attempt_id: Option<i64>,
    pub expires_ms: Option<i64>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EngineError {
    InvalidInput,
    InvalidPayload,
    UnsupportedPayload,
    Unauthorized,
    Excluded,
    NotFound,
    Ambiguous,
    Limit,
    Conflict,
    StaleAttempt,
    InvalidTransition,
    StorageUncertain,
    Schema,
    Clock,
    Entropy,
    AsyncRequired,
    Output,
}
impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "daemon operation: {self:?}")
    }
}
impl std::error::Error for EngineError {}
