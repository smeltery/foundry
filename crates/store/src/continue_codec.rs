use crate::{
    ContextCheckpointId, ContinueAcceptanceInput, ContinueReceipt, RestoreContinueCodec,
    RestoreContinueIntentV1, RestoreContinueMode, Result, conflict,
};
use serde_json::{Value, json};
pub(crate) const MAX_BODY: usize = 4096;
pub(crate) fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn hash(value: &Value) -> Result<[u8; 32]> {
    let s = value
        .as_str()
        .filter(|s| s.len() == 64)
        .ok_or_else(|| conflict("invalid acceptance digest"))?;
    let mut out = [0; 32];
    for (index, pair) in s.as_bytes().chunks_exact(2).enumerate() {
        let digit = |b| match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(conflict("invalid acceptance hex")),
        };
        out[index] = digit(pair[0])? * 16 + digit(pair[1])?;
    }
    Ok(out)
}
fn text(value: &Value) -> Result<String> {
    let s = value
        .as_str()
        .ok_or_else(|| conflict("invalid acceptance string"))?;
    crate::checkpoint_codec::identifier(s)?;
    Ok(s.into())
}
fn number(value: &Value) -> Result<u64> {
    value
        .as_u64()
        .filter(|n| *n <= i64::MAX as u64)
        .ok_or_else(|| conflict("invalid acceptance counter"))
}
pub(crate) fn authorization(a: &ContinueReceipt) -> Result<Vec<u8>> {
    let m = a
        .message
        .as_ref()
        .ok_or_else(|| conflict("bound receipt required"))?;
    for id in [&a.restore_id, &a.attempt_id, &a.checkpoint_id.to_string()] {
        crate::continue_intent::uuid(id)?;
    }
    if a.session_version == 0
        || a.session_version > i64::MAX as u64
        || a.admitted_work_revision == 0
        || a.admitted_work_revision > i64::MAX as u64
        || m.sequence == 0
        || m.sequence > i64::MAX as u64
        || !(1..=262144).contains(&m.text_bytes)
    {
        return Err(conflict("invalid authorization projection counters"));
    }
    for s in [&a.restore_id, &a.attempt_id, &a.request_id, &m.message_id] {
        crate::checkpoint_codec::identifier(s)?;
    }
    Ok(serde_json::to_vec(&json!({
        "restore_id":a.restore_id,"attempt_id":a.attempt_id,"request_id":a.request_id,
        "delivery_digest":hex(&a.delivery_digest),"checkpoint_id":a.checkpoint_id.as_str(),
        "session_version":a.session_version,"admitted_work_revision":a.admitted_work_revision,
        "message":{"message_id":m.message_id,"sequence":m.sequence,"text_digest":hex(&m.text_digest),"text_bytes":m.text_bytes}
    }))?)
}
pub(crate) fn encode(input: &ContinueAcceptanceInput) -> Result<Vec<u8>> {
    let i = &input.intent;
    i.validate()?;
    let bytes = serde_json::to_vec(&json!({
        "intent": {
            "session_id":i.session_id,"restore_id":i.restore_id,"load_id":i.load_id,"continuation_id":i.continuation_id,
            "baseline_checkpoint_id":i.baseline_checkpoint_id.as_str(),"baseline_record_digest":hex(&i.baseline_record_digest),
            "baseline_witness_digest":hex(&i.baseline_witness_digest),"baseline_validation_binding":hex(&i.baseline_validation_binding),
            "configuration_digest":hex(&i.configuration_digest),"message_id":i.message_id,"message_sequence":i.message_sequence,
            "message_text_digest":hex(&i.message_text_digest),"message_text_bytes":i.message_text_bytes,
            "mode":"queued_instruction","codec":"restore_continue_v2","tail_receipt_digest":hex(&i.tail_receipt_digest)
        }, "authorization_binding":hex(&input.authorization_binding),
        "observed_tail_receipt_digest":hex(&input.observed_tail_receipt_digest)
    }))?;
    if bytes.len() > MAX_BODY {
        return Err(conflict("acceptance body limit"));
    }
    Ok(bytes)
}
pub(crate) fn decode(
    bytes: &[u8],
    authorization: ContinueReceipt,
) -> Result<ContinueAcceptanceInput> {
    if bytes.is_empty() || bytes.len() > MAX_BODY {
        return Err(conflict("acceptance body limit"));
    }
    let (mut depth, mut string, mut escape) = (0_u32, false, false);
    for &b in bytes {
        if string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                string = false;
            }
        } else {
            match b {
                b'"' => string = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > 4 {
                        return Err(conflict("acceptance depth limit"));
                    }
                }
                b'}' | b']' => {
                    depth = depth
                        .checked_sub(1)
                        .ok_or_else(|| conflict("invalid acceptance nesting"))?
                }
                _ => {}
            }
        }
    }
    let v: Value = serde_json::from_slice(bytes)?;
    let i = &v["intent"];
    if i["mode"] != "queued_instruction" || i["codec"] != "restore_continue_v2" {
        return Err(conflict("unsupported acceptance mode or codec"));
    }
    let result = ContinueAcceptanceInput {
        authorization,
        intent: RestoreContinueIntentV1 {
            session_id: text(&i["session_id"])?,
            restore_id: text(&i["restore_id"])?,
            load_id: text(&i["load_id"])?,
            continuation_id: text(&i["continuation_id"])?,
            baseline_checkpoint_id: ContextCheckpointId::parse(&text(
                &i["baseline_checkpoint_id"],
            )?)?,
            baseline_record_digest: hash(&i["baseline_record_digest"])?,
            baseline_witness_digest: hash(&i["baseline_witness_digest"])?,
            baseline_validation_binding: hash(&i["baseline_validation_binding"])?,
            configuration_digest: hash(&i["configuration_digest"])?,
            message_id: text(&i["message_id"])?,
            message_sequence: number(&i["message_sequence"])?,
            message_text_digest: hash(&i["message_text_digest"])?,
            message_text_bytes: number(&i["message_text_bytes"])?,
            mode: RestoreContinueMode::QueuedInstruction,
            codec: RestoreContinueCodec::RestoreContinueV2,
            tail_receipt_digest: hash(&i["tail_receipt_digest"])?,
        },
        authorization_binding: hash(&v["authorization_binding"])?,
        observed_tail_receipt_digest: hash(&v["observed_tail_receipt_digest"])?,
    };
    result.validate()?;
    // Reject duplicate/unknown fields and alternate encodings, not just semantic changes.
    if encode(&result)? != bytes {
        return Err(conflict("noncanonical acceptance body"));
    }
    Ok(result)
}
pub(crate) fn event(input: &ContinueAcceptanceInput, request: [u8; 32]) -> Result<String> {
    Ok(serde_json::to_string(&json!({
        "restore_id":input.intent.restore_id,"load_id":input.intent.load_id,
        "attempt_id":input.authorization.attempt_id,"message_id":input.intent.message_id,
        "authorization_binding":hex(&input.authorization_binding),
        "tail_receipt_digest":hex(&input.observed_tail_receipt_digest),"request_digest":hex(&request)
    }))?)
}
