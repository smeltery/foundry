use super::*;
use foundry_contracts::{
    context::{ContextDigest, ContextFormat, ContextId},
    context_v2::RestoreDescriptor,
};
use foundry_runtime::coordinated::origin::*;
use foundry_store::{RestoreBaselineSnapshot, witness::LogicalRecord};
fn digest(raw: [u8; 32]) -> ContextDigest {
    ContextDigest::parse(&raw.iter().map(|b| format!("{b:02x}")).collect::<String>())
        .expect("fixed digest")
}
fn id(value: &str) -> Result<ContextId, CommandError> {
    ContextId::new(value).map_err(|_| failure("invalid baseline identity"))
}
pub(super) fn descriptor(
    snapshot: &RestoreBaselineSnapshot,
) -> Result<RestoreDescriptor, CommandError> {
    let head = snapshot.checkpoint().reference();
    let header = &snapshot.witness().body().header;
    Ok(RestoreDescriptor {
        capture_id: id(&header.source_request_id)?,
        checkpoint_id: id(head.id.as_str())?,
        record_digest: digest(head.record_digest),
        witness_digest: digest(snapshot.witness().digest()),
        format: ContextFormat {
            profile: id(&header.wire_format.profile)?,
            format_version: id(&header.wire_format.format_version)?,
            producer_build: id(&header.wire_format.producer_build)?,
        },
        configuration_digest: digest(head.configuration_digest),
        payload_length: head.payload_length,
        payload_digest: digest(head.payload_digest),
        retention_binding: digest(snapshot.witness().digest()),
    })
}
pub(super) fn origin(
    snapshot: &RestoreBaselineSnapshot,
    profile: ContextDigest,
) -> Result<OriginIdentity, CommandError> {
    let head = snapshot.checkpoint().reference();
    let witness = snapshot.witness();
    let body = witness.body();
    let Some(LogicalRecord::Prompt(prompt)) = body.records.first() else {
        return Err(failure("historical prompt absent"));
    };
    let encoded = body
        .encode()
        .map_err(|_| failure("invalid historical witness"))?;
    let commitment = ContextDigest::of(encoded.bytes());
    OriginIdentity::restored(RestoredBaselineIdentity {
        session_id: id(&head.session_id)?,
        checkpoint_id: id(head.id.as_str())?,
        generation: head.generation,
        source_attempt_id: id(&head.source_attempt_id)?,
        original_prompt_attempt_id: id(&prompt.attempt_id)?,
        prompt_digest: digest(prompt.text_digest),
        prompt_bytes: prompt.text_bytes,
        record_digest: digest(head.record_digest),
        payload_digest: digest(head.payload_digest),
        witness_digest: digest(witness.digest()),
        witness_bytes: encoded.bytes().len() as u64,
        history_binding: commitment,
        prefix_binding: commitment,
        logical_records: body.records.len() as u64,
        prefix_records: body.prefix.len() as u64,
        mapping_refs: body.mappings.len() as u64,
        profile_binding: profile,
    })
    .map_err(|_| failure("invalid historical origin"))
}
