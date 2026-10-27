use super::{
    types::*,
    wire::{Fields, Wire, choice, object, raw, tagged},
};
use crate::{CheckpointCut, ContextFormat, checkpoint_codec::invalid};
// Field order is the frozen lexical JSON key order, independent of map features.
object!(WitnessBodyV1 {
    boundaries,
    header,
    mappings,
    prefix,
    records
});
object!(WitnessHeader {
    bridge,
    comparison,
    configuration_digest,
    cut,
    parent,
    payload_digest,
    payload_length,
    session_id,
    source_attempt_id,
    source_request_id,
    stored_format,
    terminal_digest,
    trust_policy_revision,
    validation_binding,
    validator,
    wire_format
});
object!(WitnessParent {
    checkpoint_id,
    generation,
    record_digest,
    witness_digest
});
object!(WitnessWireFormat {
    format_version,
    producer_build,
    profile
});
object!(WitnessCodec { id, version });
object!(ContextFormat {
    binding_id,
    binding_version,
    format_id,
    format_version,
    profile_id
});
object!(CheckpointCut {
    event_digest,
    event_seq,
    input_digest,
    tool_digest,
    work_revision
});
object!(PromptWitness {
    attempt_id,
    session_id,
    text_bytes,
    text_digest
});
object!(InstructionWitness {
    attempt_id,
    delivery,
    message_id,
    sequence,
    text_bytes,
    text_digest
});
object!(EventIdentity {
    digest,
    id,
    revision,
    sequence
});
object!(AcceptanceIdentity { digest, restore_id });
object!(ToolIdentity {
    attempt_id,
    call_id,
    input_digest,
    tool
});
object!(ResultBodyWitness {
    bridge,
    bytes,
    digest
});
object!(AdmittedResultWitness {
    identity,
    outcome,
    output_digest,
    wire
});
object!(SyntheticResultWitness {
    failure_branch,
    identity,
    wire
});
object!(DeniedResultWitness {
    attempt_id,
    call_id,
    decision_code,
    input_bytes,
    input_digest,
    input_encoding,
    policy_revision,
    request_event,
    tool,
    wire
});
object!(PrefixRecord {
    comparison_digest,
    id,
    parent_id
});
object!(MappingReference {
    logical_index,
    native_index,
    role
});
object!(PrefixBoundary {
    completed_turn_id,
    end,
    leaf_index,
    terminal_digest
});
tagged!(LogicalRecord {Prompt=>"prompt",Instruction=>"instruction",AdmittedResult=>"admitted_result",
    SyntheticNoEffect=>"synthetic_no_effect",DeniedResult=>"denied_result"});
tagged!(DeliveryEvidence {OrdinaryEvent=>"ordinary_event",ContinueAcceptance=>"continue_acceptance"});
choice!(WitnessOutcome {Success=>"success",Failed=>"failed"});
choice!(MappingRole {Input=>"input",ToolCall=>"tool_call",ToolResult=>"tool_result"});
impl Wire for u32 {
    fn write(&self, output: &mut impl std::io::Write) -> crate::Result<()> {
        u64::from(*self).write(output)
    }
    fn read(value: serde_json::Value) -> crate::Result<Self> {
        u32::try_from(u64::read(value)?).map_err(|_| invalid("witness integer range"))
    }
}
