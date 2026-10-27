use super::{types::*, *};
use crate::{Result, checkpoint_codec::invalid};
use std::collections::BTreeSet;

fn require(condition: bool) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(invalid("invalid witness structure"))
    }
}
fn uuid(id: &str) -> Result<()> {
    require(uuid::Uuid::parse_str(id).is_ok_and(|v| v.to_string() == id))
}
fn text_bytes(bytes: u64) -> Result<()> {
    require((1..=262144).contains(&bytes))
}
fn event(event: &EventIdentity) -> Result<()> {
    require(event.sequence > 0)
}
fn tool(tool: &ToolIdentity) -> Result<()> {
    uuid(&tool.attempt_id)
}
fn wire(wire: &ResultBodyWitness) -> Result<()> {
    require(wire.bytes > 0 && wire.bytes <= 2097152)
}

pub(super) fn structure(body: &WitnessBodyV1) -> Result<()> {
    require(!body.records.is_empty() && body.records.len() <= MAX_WITNESS_LOGICAL_RECORDS)?;
    require(!body.prefix.is_empty() && body.prefix.len() <= MAX_WITNESS_PREFIX_RECORDS)?;
    require(body.mappings.len() <= MAX_WITNESS_MAPPING_REFERENCES)?;
    require(body.boundaries.len() <= body.prefix.len())?;
    let header = &body.header;
    uuid(&header.session_id)?;
    uuid(&header.source_attempt_id)?;
    require(header.stored_format.format_version > 0)?;
    require((1..=crate::MAX_CHECKPOINT_BYTES as u64).contains(&header.payload_length))?;
    if let Some(parent) = &header.parent {
        uuid(&parent.checkpoint_id)?;
        require(parent.generation > 0)?;
    }
    let mut prompts = 0;
    let mut messages = BTreeSet::new();
    let mut sequences = BTreeSet::new();
    let mut calls = BTreeSet::new();
    let mut events = BTreeSet::new();
    let mut event_sequences = BTreeSet::new();
    let mut deliveries = BTreeSet::new();
    for record in &body.records {
        match record {
            LogicalRecord::Prompt(p) => {
                prompts += 1;
                require(p.session_id == header.session_id)?;
                uuid(&p.attempt_id)?;
                text_bytes(p.text_bytes)?;
            }
            LogicalRecord::Instruction(i) => {
                uuid(&i.attempt_id)?;
                text_bytes(i.text_bytes)?;
                require(
                    i.sequence > 0
                        && messages.insert(&i.message_id)
                        && sequences.insert(i.sequence),
                )?;
                match &i.delivery {
                    DeliveryEvidence::OrdinaryEvent(e) => {
                        event(e)?;
                        require(
                            events.insert((&e.id, e.revision))
                                && event_sequences.insert(e.sequence),
                        )?;
                    }
                    DeliveryEvidence::ContinueAcceptance(a) => {
                        uuid(&a.restore_id)?;
                        require(deliveries.insert(&a.restore_id))?;
                    }
                }
            }
            LogicalRecord::AdmittedResult(a) => {
                tool(&a.identity)?;
                wire(&a.wire)?;
                require(calls.insert((&a.identity.attempt_id, &a.identity.call_id)))?;
                require(
                    !matches!(a.outcome, WitnessOutcome::Success) || a.output_digest.is_some(),
                )?;
            }
            LogicalRecord::SyntheticNoEffect(s) => {
                tool(&s.identity)?;
                wire(&s.wire)?;
                require(calls.insert((&s.identity.attempt_id, &s.identity.call_id)))?;
            }
            LogicalRecord::DeniedResult(d) => {
                uuid(&d.attempt_id)?;
                wire(&d.wire)?;
                event(&d.request_event)?;
                require(d.input_bytes > 0 && d.input_bytes <= 2097152)?;
                require(calls.insert((&d.attempt_id, &d.call_id)))?;
                require(
                    events.insert((&d.request_event.id, d.request_event.revision))
                        && event_sequences.insert(d.request_event.sequence),
                )?;
            }
        }
    }
    require(prompts == 1 && matches!(body.records[0], LogicalRecord::Prompt(_)))?;
    let mut native = BTreeSet::new();
    for record in &body.prefix {
        // A parent, when present, must be an earlier retained native ID.
        if let Some(parent) = &record.parent_id {
            require(native.contains(parent))?;
        }
        require(native.insert(&record.id))?;
    }
    let mut mappings = BTreeSet::new();
    let mut covered = BTreeSet::new();
    for m in &body.mappings {
        require(
            m.logical_index < (body.records.len() as u64)
                && m.native_index < (body.prefix.len() as u64),
        )?;
        let role = match m.role {
            MappingRole::Input => 0,
            MappingRole::ToolCall => 1,
            MappingRole::ToolResult => 2,
        };
        require(mappings.insert((m.logical_index, m.native_index, role)))?;
        let input = matches!(
            body.records[m.logical_index as usize],
            LogicalRecord::Prompt(_) | LogicalRecord::Instruction(_)
        );
        require(input == matches!(m.role, MappingRole::Input))?;
        if input || matches!(m.role, MappingRole::ToolResult) {
            covered.insert(m.logical_index);
        }
    }
    require(covered.len() == body.records.len())?;
    let mut end = 0;
    for boundary in &body.boundaries {
        require(
            boundary.end > end
                && boundary.end <= body.prefix.len() as u64
                && boundary.leaf_index < boundary.end,
        )?;
        end = boundary.end;
    }
    Ok(())
}
