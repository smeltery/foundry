use crate::catalog::{EndpointFormat, ModelId};
use serde_json::{Map, Value, json};

/// Most messages one request carries.
const MAX_MESSAGES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

/// One non-streaming text completion for an explicitly listed model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRequest {
    pub model: ModelId,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub max_output_tokens: u32,
}

impl CompletionRequest {
    pub(super) fn has_valid_messages(&self) -> bool {
        (1..=MAX_MESSAGES).contains(&self.messages.len())
    }
}

/// The reply text and the token counts the endpoint reported, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub text: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// The path appended to the base URL for each format.
pub(super) fn path(format: EndpointFormat) -> &'static str {
    match format {
        EndpointFormat::Chat => "chat/completions",
        EndpointFormat::Response => "responses",
        EndpointFormat::Message => "messages",
    }
}

/// The header that carries the key for each format.
pub(super) fn key_header(format: EndpointFormat) -> &'static str {
    match format {
        EndpointFormat::Chat | EndpointFormat::Response => "authorization",
        EndpointFormat::Message => "x-api-key",
    }
}

pub(super) fn request_body(format: EndpointFormat, request: &CompletionRequest) -> Value {
    let mut messages: Vec<Value> = request
        .messages
        .iter()
        .map(|message| json!({"role": message.role.name(), "content": message.content}))
        .collect();
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model.as_str()));
    body.insert("stream".into(), json!(false));
    match format {
        EndpointFormat::Chat => {
            if let Some(system) = &request.system {
                messages.insert(0, json!({"role": "system", "content": system}));
            }
            body.insert("messages".into(), messages.into());
            body.insert("max_tokens".into(), json!(request.max_output_tokens));
        }
        EndpointFormat::Response => {
            if let Some(system) = &request.system {
                body.insert("instructions".into(), json!(system));
            }
            body.insert("input".into(), messages.into());
            body.insert("max_output_tokens".into(), json!(request.max_output_tokens));
        }
        EndpointFormat::Message => {
            if let Some(system) = &request.system {
                body.insert("system".into(), json!(system));
            }
            body.insert("messages".into(), messages.into());
            body.insert("max_tokens".into(), json!(request.max_output_tokens));
        }
    }
    Value::Object(body)
}

/// Extracts the reply text. `None` when the reply lacks the format's text.
pub(super) fn parse_reply(format: EndpointFormat, reply: &Value) -> Option<Completion> {
    let (text, input, output) = match format {
        EndpointFormat::Chat => (
            reply
                .pointer("/choices/0/message/content")?
                .as_str()?
                .to_owned(),
            "/usage/prompt_tokens",
            "/usage/completion_tokens",
        ),
        EndpointFormat::Response => (
            joined_text(
                reply
                    .get("output")?
                    .as_array()?
                    .iter()
                    .filter(|item| item["type"] == "message")
                    .filter_map(|item| item["content"].as_array())
                    .flatten(),
                "output_text",
            )?,
            "/usage/input_tokens",
            "/usage/output_tokens",
        ),
        EndpointFormat::Message => (
            joined_text(reply.get("content")?.as_array()?.iter(), "text")?,
            "/usage/input_tokens",
            "/usage/output_tokens",
        ),
    };
    Some(Completion {
        text,
        input_tokens: reply.pointer(input).and_then(Value::as_u64),
        output_tokens: reply.pointer(output).and_then(Value::as_u64),
    })
}

/// Concatenates the `text` of every part of the given type; `None` if none.
fn joined_text<'a>(parts: impl Iterator<Item = &'a Value>, kind: &str) -> Option<String> {
    let texts: Vec<&str> = parts
        .filter(|part| part["type"] == kind)
        .filter_map(|part| part["text"].as_str())
        .collect();
    (!texts.is_empty()).then(|| texts.concat())
}
