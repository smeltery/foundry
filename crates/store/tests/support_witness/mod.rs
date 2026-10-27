use foundry_contracts::{Lifecycle, Outcome};
use foundry_store::{AttemptFence, Store, TerminalProposal, WitnessedCheckpointInput, witness::*};
use sha2::{Digest, Sha256};
#[path = "../support_checkpoint/mod.rs"]
pub mod checkpoint;
pub fn terminal() -> TerminalProposal {
    TerminalProposal {
        lifecycle: Lifecycle::Exit,
        outcome: Some(Outcome::Success),
        result: Some("done".into()),
    }
}
pub fn digest(domain: &[u8], fields: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for value in std::iter::once(domain).chain(fields.iter().copied()) {
        h.update((value.len() as u64).to_be_bytes());
        h.update(value);
    }
    h.finalize().into()
}
pub fn input(store: &Store, fence: &AttemptFence, request: &str) -> WitnessedCheckpointInput {
    let snapshot = store.capture_snapshot(fence).unwrap();
    let mut checkpoint = checkpoint::input(store, fence, request);
    checkpoint.expected_head = snapshot.head.as_ref().map(|h| h.id.clone());
    let mut witness = WitnessBodyV1::decode(include_bytes!("../fixtures/witness-v1.json")).unwrap();
    witness.records.truncate(1);
    witness.prefix.truncate(1);
    witness.mappings.truncate(1);
    witness.boundaries.clear();
    witness.header.session_id = fence.session_id.clone();
    witness.header.source_attempt_id = fence.attempt_id.clone();
    witness.header.source_request_id = request.into();
    witness.header.parent = snapshot.head.as_ref().map(|head| WitnessParent {
        checkpoint_id: head.id.to_string(),
        generation: head.generation,
        record_digest: head.record_digest,
        witness_digest: store
            .read_checkpoint_witness(&fence.session_id, &head.id)
            .unwrap()
            .digest(),
    });
    witness.header.cut = checkpoint.cut.clone();
    witness.header.stored_format = checkpoint.format.clone();
    witness.header.configuration_digest = checkpoint.configuration_digest;
    witness.header.payload_digest = Sha256::digest(&checkpoint.payload).into();
    witness.header.payload_length = checkpoint.payload.len() as u64;
    witness.header.terminal_digest = digest(
        b"foundry.capture.terminal.v1",
        &[b"exit", b"success", b"done"],
    );
    let prompt_attempt = if let Some(head) = &snapshot.head {
        match &store
            .read_checkpoint_witness(&fence.session_id, &head.id)
            .unwrap()
            .body()
            .records[0]
        {
            LogicalRecord::Prompt(p) => p.attempt_id.clone(),
            _ => unreachable!(),
        }
    } else {
        fence.attempt_id.clone()
    };
    witness.records[0] = LogicalRecord::Prompt(PromptWitness {
        session_id: fence.session_id.clone(),
        attempt_id: prompt_attempt,
        text_digest: snapshot.prompt_digest,
        text_bytes: snapshot.prompt_bytes,
    });
    WitnessedCheckpointInput {
        checkpoint,
        witness,
    }
}
