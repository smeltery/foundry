use crate::{
    CheckpointCut, ContextFormat, CoverageManifest, MAX_CHECKPOINT_BYTES,
    MAX_CHECKPOINT_COVERAGE_BYTES, MAX_CHECKPOINT_METADATA_BYTES, OpaqueCheckpointInput, Result,
    StoreError,
};
use foundry_contracts::ContractError;
use serde_json::{Value, json};

pub(crate) fn invalid(message: &str) -> StoreError {
    ContractError::Invalid(message.into()).into()
}
pub(crate) fn identifier(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(invalid("invalid checkpoint identifier"));
    }
    Ok(())
}
pub(crate) fn encode(input: &OpaqueCheckpointInput) -> Result<(String, String)> {
    identifier(&input.request_id)?;
    if input.payload.is_empty() || input.payload.len() > MAX_CHECKPOINT_BYTES {
        return Err(invalid("checkpoint payload limit"));
    }
    format_valid(&input.format)?;
    let cut = &input.cut;
    if cut.work_revision > i64::MAX as u64 || cut.event_seq > i64::MAX as u64 {
        return Err(invalid("checkpoint cut exceeds integer range"));
    }
    let format = &input.format;
    let annotation = input.producer_validation.as_ref().unwrap_or(&Value::Null);
    bounded(annotation, 0, MAX_CHECKPOINT_METADATA_BYTES)?;
    let metadata = json!({
        "format": {
            "format_id": format.format_id,
            "format_version": format.format_version,
            "profile_id": format.profile_id,
            "binding_id": format.binding_id,
            "binding_version": format.binding_version,
        },
        "cut": {
            "work_revision": cut.work_revision,
            "event_seq": cut.event_seq,
            "event_digest": cut.event_digest,
            "input_digest": cut.input_digest,
            "tool_digest": cut.tool_digest,
        },
        "producer_validation": annotation,
    });
    let metadata = canonical(metadata).to_string();
    for ids in [&input.coverage.message_ids, &input.coverage.tool_call_ids] {
        if ids.iter().any(|id| id.is_empty()) || ids.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(invalid("checkpoint coverage must have sorted unique IDs"));
        }
        if ids
            .iter()
            .try_fold(0usize, |n, id| n.checked_add(id.len()))
            .is_none_or(|n| n > MAX_CHECKPOINT_COVERAGE_BYTES)
        {
            return Err(invalid("checkpoint coverage limit"));
        }
    }
    let coverage = json!({
        "message_ids": input.coverage.message_ids,
        "tool_call_ids": input.coverage.tool_call_ids,
    })
    .to_string();
    if metadata.len() > MAX_CHECKPOINT_METADATA_BYTES
        || coverage.len() > MAX_CHECKPOINT_COVERAGE_BYTES
    {
        return Err(invalid("checkpoint metadata or coverage limit"));
    }
    Ok((metadata, coverage))
}
fn bounded(value: &Value, depth: usize, budget: usize) -> Result<usize> {
    if depth > 32 || budget == 0 {
        return Err(invalid("checkpoint annotation depth or size limit"));
    }
    let mut used = 1usize;
    match value {
        Value::String(text) => used = used.saturating_add(text.len()),
        Value::Array(values) => {
            for value in values {
                used = used.saturating_add(bounded(value, depth + 1, budget.saturating_sub(used))?);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                used = used.saturating_add(key.len());
                used = used.saturating_add(bounded(value, depth + 1, budget.saturating_sub(used))?);
            }
        }
        _ => {}
    }
    if used > budget {
        return Err(invalid("checkpoint annotation size limit"));
    }
    Ok(used)
}
fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut fields: Vec<_> = map.into_iter().collect();
            fields.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(fields.into_iter().map(|(k, v)| (k, canonical(v))).collect())
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
        value => value,
    }
}
pub(crate) fn decode(
    metadata: &str,
    coverage: &str,
) -> Result<(
    ContextFormat,
    CheckpointCut,
    CoverageManifest,
    Option<Value>,
)> {
    let value: Value = serde_json::from_str(metadata)?;
    let f = &value["format"];
    let c = &value["cut"];
    let text = |key| {
        f[key]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| invalid("invalid checkpoint format"))
    };
    let format = ContextFormat {
        format_id: text("format_id")?,
        format_version: u32::try_from(
            f["format_version"]
                .as_u64()
                .ok_or_else(|| invalid("invalid format version"))?,
        )
        .map_err(|_| invalid("invalid format version"))?,
        profile_id: text("profile_id")?,
        binding_id: text("binding_id")?,
        binding_version: text("binding_version")?,
    };
    format_valid(&format)?;
    let counter = |key| {
        c[key]
            .as_u64()
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| invalid("invalid checkpoint cut"))
    };
    let bytes = |key| serde_json::from_value::<[u8; 32]>(c[key].clone()).map_err(StoreError::Json);
    let cut = CheckpointCut {
        work_revision: counter("work_revision")?,
        event_seq: counter("event_seq")?,
        event_digest: bytes("event_digest")?,
        input_digest: bytes("input_digest")?,
        tool_digest: bytes("tool_digest")?,
    };
    let coverage: Value = serde_json::from_str(coverage)?;
    let manifest = CoverageManifest {
        message_ids: serde_json::from_value(coverage["message_ids"].clone())?,
        tool_call_ids: serde_json::from_value(coverage["tool_call_ids"].clone())?,
    };
    let annotation = value["producer_validation"].clone();
    Ok((
        format,
        cut,
        manifest,
        if annotation.is_null() {
            None
        } else {
            Some(annotation)
        },
    ))
}
fn format_valid(format: &ContextFormat) -> Result<()> {
    for field in [
        &format.format_id,
        &format.profile_id,
        &format.binding_id,
        &format.binding_version,
    ] {
        identifier(field)?;
    }
    if format.format_version == 0 {
        return Err(invalid("format version must be positive"));
    }
    Ok(())
}

impl OpaqueCheckpointInput {
    /// Canonical exact-request identity for retained-input reconciliation. This
    /// pure computation does not verify fence ownership or native semantics.
    pub fn request_digest(&self, fence: &crate::AttemptFence) -> Result<[u8; 32]> {
        self.digest_with_witness(fence, None, None)
    }

    /// Exact atomic-finalization identity, including the terminal proposal.
    pub fn finalization_request_digest(
        &self,
        fence: &crate::AttemptFence,
        terminal: &crate::TerminalProposal,
    ) -> Result<[u8; 32]> {
        self.digest_with_witness(fence, Some(terminal), None)
    }

    pub(crate) fn digest_with_witness(
        &self,
        fence: &crate::AttemptFence,
        terminal: Option<&crate::TerminalProposal>,
        witness: Option<([u8; 32], usize)>,
    ) -> Result<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let (metadata, coverage) = encode_with_terminal(self, terminal)?;
        let payload_digest: [u8; 32] = Sha256::digest(&self.payload).into();
        Ok(request_hash(
            [
                fence.session_id.as_bytes(),
                fence.attempt_id.as_bytes(),
                self.request_id.as_bytes(),
                self.expected_head
                    .as_ref()
                    .map_or("", |id| id.as_str())
                    .as_bytes(),
                metadata.as_bytes(),
                coverage.as_bytes(),
                &self.configuration_digest,
                &payload_digest,
                &(self.payload.len() as u64).to_be_bytes(),
            ],
            witness,
        ))
    }
}

pub(crate) fn encode_with_terminal(
    input: &OpaqueCheckpointInput,
    terminal: Option<&crate::TerminalProposal>,
) -> Result<(String, String)> {
    let (metadata, coverage) = encode(input)?;
    let Some(terminal) = terminal else {
        return Ok((metadata, coverage));
    };
    crate::attempts::validate_terminal(terminal.lifecycle, terminal.outcome)?;
    if terminal
        .result
        .as_ref()
        .is_some_and(|text| text.len() > MAX_CHECKPOINT_METADATA_BYTES)
    {
        return Err(invalid("checkpoint terminal result limit"));
    }
    let mut value: Value = serde_json::from_str(&metadata)?;
    value["terminal"] = json!({
        "lifecycle": terminal.lifecycle, "outcome": terminal.outcome, "result": terminal.result,
    });
    let metadata = canonical(value).to_string();
    if metadata.len() > MAX_CHECKPOINT_METADATA_BYTES {
        return Err(invalid(
            "checkpoint metadata including terminal exceeds limit",
        ));
    }
    Ok((metadata, coverage))
}

pub(crate) fn decode_terminal(metadata: &str) -> Result<crate::TerminalProposal> {
    let value: Value = serde_json::from_str(metadata)?;
    let terminal = value
        .get("terminal")
        .ok_or_else(|| invalid("checkpoint has no terminal proposal"))?;
    let proposal = crate::TerminalProposal {
        lifecycle: serde_json::from_value(terminal["lifecycle"].clone())?,
        outcome: serde_json::from_value(terminal["outcome"].clone())?,
        result: serde_json::from_value(terminal["result"].clone())?,
    };
    crate::attempts::validate_terminal(proposal.lifecycle, proposal.outcome)?;
    Ok(proposal)
}

pub(crate) fn request_hash(fields: [&[u8]; 9], witness: Option<([u8; 32], usize)>) -> [u8; 32] {
    if let Some((digest, length)) = witness {
        let version = 1_u64.to_be_bytes();
        let length = (length as u64).to_be_bytes();
        let mut fields = fields.to_vec();
        fields.extend([version.as_slice(), digest.as_slice(), length.as_slice()]);
        crate::checkpoint_cut::digest(b"foundry.checkpoint.request.v2", &fields)
    } else {
        crate::checkpoint_cut::digest(b"foundry.checkpoint.request.v1", &fields)
    }
}
