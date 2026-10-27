use crate::{
    AttemptFence, Result, ToolOutcome, conflict,
    witness::*,
    witness_events::{self, EvidenceEvent},
};
use rusqlite::{Connection, params};
use std::collections::BTreeSet;
fn require(valid: bool) -> Result<()> {
    if valid {
        Ok(())
    } else {
        Err(conflict("witness differs from durable evidence"))
    }
}
fn event_matches(event: &EvidenceEvent, identity: &EventIdentity, attempt: &str) -> bool {
    event.id == identity.id
        && event.sequence == identity.sequence
        && event.revision == identity.revision
        && event.attempt == attempt
        && event.digest == identity.digest
}
fn tool_event(event: &EvidenceEvent, identity: &ToolIdentity) -> bool {
    event.kind == "tool_request"
        && event.attempt == identity.attempt_id
        && event.subject == identity.call_id
        && event.tool.as_deref() == Some(identity.tool.as_str())
}
pub(crate) fn verify(
    connection: &Connection,
    fence: &AttemptFence,
    body: &WitnessBodyV1,
) -> Result<()> {
    let snapshot = crate::capture_snapshot::snapshot(connection, &fence.session_id)?;
    require(snapshot.cut == body.header.cut)?;
    let events = witness_events::selected(connection, &fence.session_id)?;
    let mut messages = BTreeSet::new();
    let mut tools = BTreeSet::new();
    let mut explained = BTreeSet::new();
    for record in &body.records {
        match record {
            LogicalRecord::Prompt(p) => {
                require(
                    p.session_id == fence.session_id
                        && p.text_digest == snapshot.prompt_digest
                        && p.text_bytes == snapshot.prompt_bytes,
                )?;
                let exists: bool = connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM attempts WHERE session_id=?1 AND id=?2)",
                    params![fence.session_id, p.attempt_id],
                    |r| r.get(0),
                )?;
                require(exists)?;
            }
            LogicalRecord::Instruction(i) => {
                let m = snapshot
                    .messages
                    .iter()
                    .find(|m| m.id == i.message_id)
                    .ok_or_else(|| conflict("instruction has no delivered durable message"))?;
                require(
                    m.sequence == i.sequence
                        && m.attempt_id == i.attempt_id
                        && m.text_digest == i.text_digest
                        && m.text_bytes == i.text_bytes,
                )?;
                messages.insert(i.message_id.as_str());
                let DeliveryEvidence::OrdinaryEvent(identity) = &i.delivery else {
                    return Err(foundry_contracts::ContractError::Unsupported(
                        "Continue acceptance witness is not available".into(),
                    )
                    .into());
                };
                let event = events
                    .iter()
                    .find(|event| {
                        event.kind == "message_accepted"
                            && event.subject == i.message_id
                            && event_matches(event, identity, &i.attempt_id)
                    })
                    .ok_or_else(|| {
                        foundry_contracts::ContractError::Unsupported(
                            "ordinary instruction acceptance evidence missing".into(),
                        )
                    })?;
                explained.insert(event.sequence);
            }
            LogicalRecord::AdmittedResult(result) => {
                let outcome = match result.outcome {
                    WitnessOutcome::Success => ToolOutcome::Success,
                    WitnessOutcome::Failed => ToolOutcome::Failed,
                };
                verify_tool(&snapshot, &result.identity, outcome, result.output_digest)?;
                tools.insert(result.identity.call_id.as_str());
                require(events.iter().any(|e| tool_event(e, &result.identity)))?;
                for e in events.iter().filter(|e| tool_event(e, &result.identity)) {
                    explained.insert(e.sequence);
                }
            }
            LogicalRecord::SyntheticNoEffect(result) => {
                verify_tool(&snapshot, &result.identity, ToolOutcome::Failed, None)?;
                tools.insert(result.identity.call_id.as_str());
                require(events.iter().any(|e| tool_event(e, &result.identity)))?;
                for e in events.iter().filter(|e| tool_event(e, &result.identity)) {
                    explained.insert(e.sequence);
                }
            }
            LogicalRecord::DeniedResult(d) => {
                require(
                    !snapshot
                        .tools
                        .iter()
                        .any(|t| t.call_id == d.call_id && t.attempt_id == d.attempt_id),
                )?;
                let event = events
                    .iter()
                    .find(|e| {
                        e.kind == "tool_request"
                            && e.subject == d.call_id
                            && e.tool.as_deref() == Some(&d.tool)
                            && event_matches(e, &d.request_event, &d.attempt_id)
                    })
                    .ok_or_else(|| conflict("denial request evidence missing"))?;
                require(
                    event.encoding.as_deref() == Some(d.input_encoding.id.as_str())
                        && d.input_encoding.version == "1"
                        && event.input_digest == Some(d.input_digest)
                        && event.input_bytes == Some(d.input_bytes),
                )?;
                explained.insert(event.sequence);
            }
        }
    }
    require(
        messages
            .into_iter()
            .eq(snapshot.coverage.message_ids.iter().map(String::as_str))
            && tools
                .into_iter()
                .eq(snapshot.coverage.tool_call_ids.iter().map(String::as_str))
            && explained.len() == events.len(),
    )
}
fn verify_tool(
    snapshot: &crate::CaptureSnapshot,
    identity: &ToolIdentity,
    outcome: ToolOutcome,
    output: Option<[u8; 32]>,
) -> Result<()> {
    let tool = snapshot
        .tools
        .iter()
        .find(|t| t.call_id == identity.call_id)
        .ok_or_else(|| conflict("result has no durable tool"))?;
    require(
        tool.attempt_id == identity.attempt_id
            && tool.tool == identity.tool
            && tool.input_digest == identity.input_digest
            && tool.outcome == outcome
            && tool.output_digest == output,
    )
}
