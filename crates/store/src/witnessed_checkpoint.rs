use crate::{
    AttemptFence, CheckpointRef, ContextCheckpointId, FinalizedCheckpoint, OpaqueCheckpointInput,
    Result, Store, TerminalProposal, checkpoint_read, checkpoints, conflict,
    witness::WitnessBodyV1,
    witness_storage::{self, StoredWitness},
};

/// Caller assertions only; publication verifies durable facts, not native semantics.
pub struct WitnessedCheckpointInput {
    pub checkpoint: OpaqueCheckpointInput,
    pub witness: WitnessBodyV1,
}
impl WitnessedCheckpointInput {
    pub fn finalization_request_digest(
        &self,
        fence: &AttemptFence,
        terminal: &TerminalProposal,
    ) -> Result<[u8; 32]> {
        let encoded = self.witness.encode()?;
        witness_storage::check_input(&self.witness, fence, &self.checkpoint, terminal)?;
        self.checkpoint.digest_with_witness(
            fence,
            Some(terminal),
            Some((encoded.digest(), encoded.bytes().len())),
        )
    }
}

/// Private historical evidence. Neither integrity nor this type grants restore authority.
pub struct PrivateWitness {
    reference: CheckpointRef,
    body: WitnessBodyV1,
    digest: [u8; 32],
}
impl PrivateWitness {
    pub fn reference(&self) -> &CheckpointRef {
        &self.reference
    }
    pub fn body(&self) -> &WitnessBodyV1 {
        &self.body
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}
impl Store {
    pub fn finish_with_witnessed_checkpoint(
        &mut self,
        fence: &AttemptFence,
        input: WitnessedCheckpointInput,
        terminal: TerminalProposal,
    ) -> Result<FinalizedCheckpoint> {
        let encoded = input.witness.encode()?;
        witness_storage::check_input(&input.witness, fence, &input.checkpoint, &terminal)?;
        let request = input.checkpoint.request_id.clone();
        let mut prepared = checkpoints::prepare(fence, input.checkpoint, Some(&terminal))?;
        prepared.row.request_format = 2;
        prepared.row.witness = Some(StoredWitness {
            body: input.witness,
            encoded,
        });
        prepared.row.request_digest = prepared.row.request_hash();
        self.finish_prepared_checkpoint(fence, &request, prepared, terminal)
    }
    pub fn read_checkpoint_witness(
        &self,
        session: &str,
        id: &ContextCheckpointId,
    ) -> Result<PrivateWitness> {
        let tx = self.connection.unchecked_transaction()?;
        let row = checkpoint_read::load(&tx, session, id.as_str())?
            .ok_or_else(|| conflict("checkpoint missing"))?;
        let (private, witness) = row.private_and_witness()?;
        let witness = witness.ok_or_else(|| {
            foundry_contracts::ContractError::Unsupported("checkpoint has no witness".into())
        })?;
        let reference = private.reference;
        let result = PrivateWitness {
            reference,
            body: witness.body,
            digest: witness.encoded.digest(),
        };
        tx.commit()?;
        Ok(result)
    }
}
