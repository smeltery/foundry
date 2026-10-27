use crate::{CheckpointCut, ContextFormat};

/// Unverified caller-supplied evidence. No native semantic authority is conferred.
pub struct WitnessBodyV1 {
    pub header: WitnessHeader,
    pub records: Vec<LogicalRecord>,
    pub prefix: Vec<PrefixRecord>,
    pub mappings: Vec<MappingReference>,
    pub boundaries: Vec<PrefixBoundary>,
}
pub struct WitnessHeader {
    pub session_id: String,
    pub source_attempt_id: String,
    pub source_request_id: String,
    pub parent: Option<WitnessParent>,
    pub stored_format: ContextFormat,
    pub wire_format: WitnessWireFormat,
    pub configuration_digest: [u8; 32],
    pub cut: CheckpointCut,
    pub payload_digest: [u8; 32],
    pub payload_length: u64,
    pub validator: WitnessCodec,
    pub bridge: WitnessCodec,
    pub comparison: WitnessCodec,
    pub trust_policy_revision: String,
    pub validation_binding: [u8; 32],
    pub terminal_digest: [u8; 32],
}
pub struct WitnessParent {
    pub checkpoint_id: String,
    pub generation: u64,
    pub record_digest: [u8; 32],
    pub witness_digest: [u8; 32],
}
pub struct WitnessWireFormat {
    pub profile: String,
    pub format_version: String,
    pub producer_build: String,
}
pub struct WitnessCodec {
    pub id: String,
    pub version: String,
}
pub enum LogicalRecord {
    Prompt(PromptWitness),
    Instruction(InstructionWitness),
    AdmittedResult(AdmittedResultWitness),
    SyntheticNoEffect(SyntheticResultWitness),
    DeniedResult(DeniedResultWitness),
}
pub struct PromptWitness {
    pub session_id: String,
    pub attempt_id: String,
    pub text_digest: [u8; 32],
    pub text_bytes: u64,
}
pub struct InstructionWitness {
    pub message_id: String,
    pub sequence: u64,
    pub attempt_id: String,
    pub text_digest: [u8; 32],
    pub text_bytes: u64,
    pub delivery: DeliveryEvidence,
}
pub enum DeliveryEvidence {
    OrdinaryEvent(EventIdentity),
    ContinueAcceptance(AcceptanceIdentity),
}
pub struct EventIdentity {
    pub id: String,
    pub sequence: u64,
    pub revision: u64,
    pub digest: [u8; 32],
}
pub struct AcceptanceIdentity {
    pub restore_id: String,
    pub digest: [u8; 32],
}
pub struct ToolIdentity {
    pub attempt_id: String,
    pub call_id: String,
    pub tool: String,
    pub input_digest: [u8; 32],
}
pub struct ResultBodyWitness {
    pub digest: [u8; 32],
    pub bytes: u64,
    pub bridge: WitnessCodec,
}
pub enum WitnessOutcome {
    Success,
    Failed,
}
pub struct AdmittedResultWitness {
    pub identity: ToolIdentity,
    pub outcome: WitnessOutcome,
    pub output_digest: Option<[u8; 32]>,
    pub wire: ResultBodyWitness,
}
/// This variant fixes the ledger fact to Failed/None. The branch is an assertion,
/// never inferred from an absent output digest by the codec.
pub struct SyntheticResultWitness {
    pub identity: ToolIdentity,
    pub failure_branch: WitnessCodec,
    pub wire: ResultBodyWitness,
}
pub struct DeniedResultWitness {
    pub attempt_id: String,
    pub call_id: String,
    pub tool: String,
    pub request_event: EventIdentity,
    pub input_encoding: WitnessCodec,
    pub input_digest: [u8; 32],
    pub input_bytes: u64,
    pub decision_code: String,
    pub policy_revision: String,
    pub wire: ResultBodyWitness,
}
pub struct PrefixRecord {
    pub id: String,
    pub parent_id: Option<String>,
    pub comparison_digest: [u8; 32],
}
pub enum MappingRole {
    Input,
    ToolCall,
    ToolResult,
}
pub struct MappingReference {
    pub logical_index: u64,
    pub native_index: u64,
    pub role: MappingRole,
}
pub struct PrefixBoundary {
    /// Exclusive prefix boundary; leaf_index must be below it.
    pub end: u64,
    pub leaf_index: u64,
    pub completed_turn_id: String,
    pub terminal_digest: [u8; 32],
}
