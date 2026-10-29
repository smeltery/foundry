use super::{Blob, Error, Result, facts};
use crate::native_publication::NativePublicationJob;
use foundry_runtime::coordinated::capture::{ProfileBinding, ResultProvenance, TraceKind};
use serde_json::{Value, json};

pub(super) fn profile(p: &ProfileBinding) -> Value {
    let f = &p.stored;
    json!({"stored":{"format_id":f.format_id,"format_version":f.format_version,"profile_id":f.profile_id,"binding_id":f.binding_id,"binding_version":f.binding_version},"wire":p.wire,"configuration_digest":facts::digest(p.configuration_digest)})
}
pub(super) fn publication(job: &NativePublicationJob) -> Result<Value> {
    let runtime = job.runtime_job();
    let identity = runtime.cut().identity();
    let cut = runtime.cut().facts();
    let correlation = runtime.correlation();
    let trace=runtime.trace().entries().iter().map(|entry| {
        let (kind,provenance)=match entry.kind() {
            TraceKind::OpeningPrompt => ("opening_prompt",Value::Null),
            TraceKind::Instruction => ("instruction",Value::Null),
            TraceKind::ToolResult(source) => ("tool_result",match source {
                ResultProvenance::PersistedHelper{ledger_output_digest} => json!({"kind":"persisted_helper","ledger_output_digest":facts::digest(*ledger_output_digest)}),
                ResultProvenance::SyntheticNoEffect => json!({"kind":"synthetic_no_effect"}),
                ResultProvenance::Denied => json!({"kind":"denied"}),
            }),
        };
        Ok(json!({"kind":kind,"attempt_id":entry.attempt_id(),"id":entry.id(),"body":Blob::new(entry.bytes(),1048576)?,"wire_digest":entry.wire_digest(),"instruction_durable":entry.instruction_is_durable(),"provenance":provenance}))
    }).collect::<Result<Vec<_>>>()?;
    let restored=runtime.restored_origin().map(|r|json!({"binding":{"observation":r.binding.observation,"instance_id":r.binding.instance_id,"effective_configuration_digest":r.binding.effective_configuration_digest},"continuation":r.continuation}));
    let acceptance=job.acceptance().map(|a|json!({"session_id":a.session_id,"restore_id":a.restore_id,"attempt_id":a.attempt_id,"message_id":a.message_id,"request_digest":facts::digest(a.request_digest),"record_digest":facts::digest(a.record_digest),"event_id":a.event_id,"event_sequence":a.event_sequence,"committed_ms":a.committed_ms}));
    let coverage =
        serde_json::to_value(runtime.candidate().coverage()).map_err(|_| Error::Invalid)?;
    facts::bounded(&coverage, 65536)?;
    Ok(
        json!({"input_digest":job.input_digest(),"runtime_binding":runtime.binding_digest(),
        "correlation":{"job_id":correlation.job.as_uuid().to_string(),"attempt_id":correlation.attempt_id,"capture_id":correlation.capture_id},
        "identity":{"session_id":identity.session_id,"attempt_id":identity.attempt_id,"request_id":identity.request_id,"expected_head":identity.expected_head.as_ref().map(|h|json!({"checkpoint_id":h.checkpoint_id,"generation":h.generation}))},
        "cut":{"work_revision":cut.work_revision,"event_seq":cut.event_seq,"event_digest":facts::digest(cut.event_digest),"input_digest":facts::digest(cut.input_digest),"tool_digest":facts::digest(cut.tool_digest)},
        "cut_binding":runtime.cut().binding_digest(),"profile":profile(runtime.cut().profile()),
        "durable_coverage":{"message_ids":runtime.cut().coverage().message_ids,"tool_call_ids":runtime.cut().coverage().tool_call_ids},
        "candidate":{"format":runtime.candidate().format(),"coverage":coverage,"payload":Blob::new(runtime.candidate().payload(),1048576)?},
        "trace":trace,"terminal":{"lifecycle":runtime.terminal().lifecycle(),"outcome":runtime.terminal().outcome(),"result":Blob::new(runtime.terminal().result().as_bytes(),16384)?,"binding_digest":runtime.terminal().binding_digest()},
        "restored":restored,"snapshot":facts::snapshot(job.snapshot())?,"events":facts::events(job.events())?,
        "baseline":job.baseline().map(facts::baseline).transpose()?,"acceptance":acceptance,
        "policy":Blob::new(job.policy(),65536)?,"associations":Blob::new(job.associations(),262144)?}),
    )
}
