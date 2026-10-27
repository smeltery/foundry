use crate::{Result, ServiceError, tool_digest};
use foundry_contracts::AdapterEvent;
use foundry_store::{AttemptFence, Store};
use uuid::Uuid;

pub(crate) fn persist_event(
    store: &mut Store,
    fence: &AttemptFence,
    event: &AdapterEvent,
) -> Result<()> {
    let sanitized;
    let event = if let AdapterEvent::Failed { attempt_id, .. } = event {
        sanitized = AdapterEvent::Failed {
            attempt_id: attempt_id.clone(),
            code: "adapter_failed".into(),
            message: "Adapter reported failure; details withheld".into(),
        };
        &sanitized
    } else {
        event
    };
    let kind = match event {
        AdapterEvent::Ready { .. } => "adapter_ready",
        AdapterEvent::Text { .. } => "text",
        AdapterEvent::ToolCall { .. } => "tool_request",
        AdapterEvent::Settled { .. } => "adapter_settled",
        AdapterEvent::Failed { .. } => "adapter_failed",
        AdapterEvent::MessageAccepted { .. } => "message_accepted",
    };
    let data = match event {
        AdapterEvent::ToolCall {
            attempt_id,
            call_id,
            name,
            input,
        } => {
            if attempt_id != &fence.attempt_id
                || !identifier(attempt_id, 128)
                || !identifier(call_id, 128)
                || !identifier(name, 64)
            {
                return Err(ServiceError::Invalid("invalid tool event identity"));
            }
            let digest = tool_digest::event_input(input)?;
            serde_json::json!({
                "type": "tool_call", "attempt_id": attempt_id,
                "call_id": call_id, "name": name,
                "input_encoding": "serde_json.compact.v1",
                "input_bytes": digest.bytes, "input_sha256": digest.sha256,
            })
        }
        _ => serde_json::to_value(event).map_err(foundry_store::StoreError::Json)?,
    };
    let event_id = match event {
        AdapterEvent::MessageAccepted { message_id, .. } => {
            format!("message-accepted:{message_id}")
        }
        _ => Uuid::new_v4().to_string(),
    };
    store.append_event(fence, &event_id, 0, kind, data)?;
    if let AdapterEvent::MessageAccepted { message_id, .. } = event {
        store.acknowledge_message(fence, message_id)?;
    }
    Ok(())
}

fn identifier(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundry_contracts::CreateSession;
    use serde_json::json;

    #[test]
    fn invalid_tool_identity_is_rejected_without_persisting_or_echoing_payload() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(directory.path().join("db")).unwrap();
        let session = store
            .create(CreateSession {
                prompt: "test".into(),
                directory: directory.path().display().to_string(),
                model: None,
                idempotency_key: None,
            })
            .unwrap();
        let fence = store.begin_attempt(&session.id, 0).unwrap();
        let cases = [
            ("foreign".into(), "call".into(), "write".into()),
            (fence.attempt_id.clone(), "".into(), "write".into()),
            (fence.attempt_id.clone(), "x".repeat(129), "write".into()),
            (fence.attempt_id.clone(), "call\n".into(), "write".into()),
            (fence.attempt_id.clone(), "call".into(), " ".into()),
            (fence.attempt_id.clone(), "call".into(), "x".repeat(65)),
        ];
        for (attempt_id, call_id, name) in cases {
            let event = AdapterEvent::ToolCall {
                attempt_id,
                call_id,
                name,
                input: json!({"path":"private-path-marker","content":"private-content-marker"}),
            };
            let error = persist_event(&mut store, &fence, &event).unwrap_err();
            assert_eq!(
                error.to_string(),
                "invalid service configuration: invalid tool event identity"
            );
        }
        assert!(store.events(&session.id, 0, 100).unwrap().is_empty());
    }

    #[test]
    fn ordinary_text_payload_is_not_subject_to_tool_input_redaction() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(directory.path().join("db")).unwrap();
        let session = store
            .create(CreateSession {
                prompt: "test".into(),
                directory: directory.path().display().to_string(),
                model: None,
                idempotency_key: None,
            })
            .unwrap();
        let fence = store.begin_attempt(&session.id, 0).unwrap();
        let event = AdapterEvent::Text {
            attempt_id: fence.attempt_id.clone(),
            text: "literal text remains".into(),
        };
        persist_event(&mut store, &fence, &event).unwrap();
        assert_eq!(
            store.events(&session.id, 0, 100).unwrap()[0].data,
            serde_json::to_value(event).unwrap()
        );
    }
}
