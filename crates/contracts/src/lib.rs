//! Shared, versioned session and subprocess interfaces.
pub mod capability;
pub mod context;
pub mod context_observation;
pub mod context_v2;
pub mod permission;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MESSAGE_ACK_CAPABILITY: &str = "message_ack_v2";
pub const MAX_PROMPT_BYTES: usize = 256 * 1024;
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Queued,
    Running,
    Exit,
    Error,
    Suspended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Failure,
    ActionRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelMode {
    Graceful,
    Hard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelConfig {
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSession {
    pub prompt: String,
    pub directory: String,
    pub model: Option<ModelConfig>,
    pub idempotency_key: Option<String>,
}

impl CreateSession {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.prompt.trim().is_empty() || self.prompt.len() > MAX_PROMPT_BYTES {
            return Err(ContractError::Invalid(
                "prompt must contain 1 to 262144 UTF-8 bytes".into(),
            ));
        }
        if self.directory.trim().is_empty() {
            return Err(ContractError::Invalid(
                "working directory is required".into(),
            ));
        }
        if self
            .idempotency_key
            .as_ref()
            .is_some_and(|key| key.trim().is_empty())
        {
            return Err(ContractError::Invalid(
                "idempotency key must not be empty".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub prompt: String,
    pub directory: String,
    pub model: Option<ModelConfig>,
    pub lifecycle: Lifecycle,
    pub outcome: Option<Outcome>,
    pub result: Option<String>,
    pub active_attempt_id: Option<String>,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub session_id: String,
    pub attempt_id: String,
    pub sequence: u64,
    pub revision: u64,
    pub kind: String,
    pub data: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageState {
    Pending,
    InFlight,
    Delivered,
    Withdrawn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub sequence: u64,
    pub text: String,
    pub state: MessageState,
    pub attempt_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdapterCommand {
    Start {
        version: u32,
        attempt_id: String,
        session: Session,
    },
    Message {
        attempt_id: String,
        message_id: String,
        text: String,
        receipt: String,
    },
    Cancel {
        attempt_id: String,
        mode: CancelMode,
    },
    ToolResult {
        attempt_id: String,
        call_id: String,
        result: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AdapterEvent {
    Ready {
        version: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
    },
    MessageAccepted {
        attempt_id: String,
        message_id: String,
        receipt: String,
    },
    Text {
        attempt_id: String,
        text: String,
    },
    ToolCall {
        attempt_id: String,
        call_id: String,
        name: String,
        input: Value,
    },
    Settled {
        attempt_id: String,
        outcome: Outcome,
        result: String,
    },
    Failed {
        attempt_id: String,
        code: String,
        message: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ContractError {
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unsupported capability: {0}")]
    Unsupported(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_empty_or_oversized_prompt() {
        let mut request = CreateSession {
            prompt: " ".into(),
            directory: "/tmp".into(),
            model: None,
            idempotency_key: None,
        };
        assert!(request.validate().is_err());
        request.prompt = "x".repeat(MAX_PROMPT_BYTES);
        assert!(request.validate().is_ok());
        request.prompt.push('x');
        assert!(request.validate().is_err());
    }

    #[test]
    fn completion_outcome_is_distinct_from_lifecycle() {
        assert_eq!(serde_json::to_string(&Lifecycle::Exit).unwrap(), "\"exit\"");
        assert_eq!(
            serde_json::to_string(&Outcome::Failure).unwrap(),
            "\"failure\""
        );
    }
}
