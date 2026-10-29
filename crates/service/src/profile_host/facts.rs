use super::{Blob, Error, Result};
use foundry_contracts::context::ContextDigest;
use foundry_store::{
    CaptureSnapshot, CheckpointCut, CheckpointRef, RestoreBaselineSnapshot, RestoreEventEvidence,
    ToolOutcome,
};
use serde_json::{Value, json};

pub(super) fn digest(bytes: [u8; 32]) -> ContextDigest {
    ContextDigest::parse(&bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
        .expect("fixed digest")
}
pub(super) fn cut(value: &CheckpointCut) -> Value {
    json!({
        "work_revision": value.work_revision, "event_seq": value.event_seq,
        "event_digest": digest(value.event_digest), "input_digest": digest(value.input_digest),
        "tool_digest": digest(value.tool_digest)
    })
}
pub(super) fn head(value: &CheckpointRef) -> Value {
    let format = &value.format;
    json!({
        "id": value.id.as_str(), "session_id": value.session_id,
        "source_attempt_id": value.source_attempt_id,
        "parent_id": value.parent_id.as_ref().map(|id| id.as_str()),
        "generation": value.generation,
        "format": {
            "format_id": format.format_id, "format_version": format.format_version,
            "profile_id": format.profile_id, "binding_id": format.binding_id,
            "binding_version": format.binding_version
        },
        "cut": cut(&value.cut), "configuration_digest": digest(value.configuration_digest),
        "payload_digest": digest(value.payload_digest), "request_digest": digest(value.request_digest),
        "record_digest": digest(value.record_digest), "payload_length": value.payload_length,
        "committed_ms": value.committed_ms
    })
}
pub(super) fn snapshot(value: &CaptureSnapshot) -> Result<Value> {
    if value.messages.len().saturating_add(value.tools.len()) >= 1024 {
        return Err(Error::Invalid);
    }
    let messages: Vec<_> = value
        .messages
        .iter()
        .map(|message| {
            json!({
                "id": message.id, "sequence": message.sequence, "attempt_id": message.attempt_id,
                "text_digest": digest(message.text_digest), "text_bytes": message.text_bytes
            })
        })
        .collect();
    let tools = value
        .tools
        .iter()
        .map(|tool| {
            let outcome = match tool.outcome {
                ToolOutcome::Success if tool.output_digest.is_some() => "success",
                ToolOutcome::Failed => "failed",
                ToolOutcome::Success | ToolOutcome::Uncertain => return Err(Error::Invalid),
            };
            Ok(json!({
                "call_id": tool.call_id, "attempt_id": tool.attempt_id, "tool": tool.tool,
                "input_digest": digest(tool.input_digest), "outcome": outcome,
                "output_digest": tool.output_digest.map(digest)
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let result = json!({
        "head": value.head.as_ref().map(head), "cut": cut(&value.cut),
        "coverage": {"message_ids": value.coverage.message_ids, "tool_call_ids": value.coverage.tool_call_ids},
        "prompt_digest": digest(value.prompt_digest), "prompt_bytes": value.prompt_bytes,
        "messages": messages, "tools": tools
    });
    bounded(&result, 524288)?;
    Ok(result)
}
pub(super) fn events(values: &[RestoreEventEvidence]) -> Result<Value> {
    if values.len() > 1024 {
        return Err(Error::Invalid);
    }
    let result = Value::Array(
        values
            .iter()
            .map(|event| {
                json!({
                    "id": event.id, "sequence": event.sequence, "revision": event.revision,
                    "attempt": event.attempt, "kind": event.kind, "digest": digest(event.digest),
                    "data_bytes": event.data_bytes, "subject": event.subject, "tool": event.tool,
                    "encoding": event.encoding, "input_digest": event.input_digest.map(digest),
                    "input_bytes": event.input_bytes
                })
            })
            .collect(),
    );
    bounded(&result, 262144)?;
    Ok(result)
}
pub(super) fn baseline(value: &RestoreBaselineSnapshot) -> Result<Value> {
    let witness = value
        .witness()
        .body()
        .encode()
        .map_err(|_| Error::Invalid)?;
    Ok(json!({
        "head": head(value.checkpoint().reference()),
        "payload": Blob::new(value.checkpoint().payload(), 1048576)?,
        "witness": Blob::new(witness.bytes(), 262144)?,
        "facts": snapshot(value.facts())?, "events": events(value.events())?
    }))
}
pub(super) fn bounded(value: &Value, limit: usize) -> Result<()> {
    super::json::encode(value, limit).map(|_| ())
}
