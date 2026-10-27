use crate::{
    AttemptFence, CheckpointRef, OpaqueCheckpointInput, Result, Store, attempts, checkpoint_codec,
    checkpoint_cut, checkpoint_read, checkpoints, conflict, read_session,
};
use foundry_contracts::{Lifecycle, Outcome};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalProposal {
    pub lifecycle: Lifecycle,
    pub outcome: Option<Outcome>,
    pub result: Option<String>,
}

/// Immutable historical finalization, not the session's current state or restore authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedCheckpoint {
    pub checkpoint: CheckpointRef,
    pub terminal: TerminalProposal,
    pub session_version: u64,
}

impl Store {
    /// Atomically publish opaque state and finish its live, uncancelled owner.
    /// Failure leaves all writes uncommitted; ordinary error/suspension cleanup remains available.
    /// After deactivation, use finished_checkpoint_request for read-only exact recovery.
    pub fn finish_with_opaque_checkpoint(
        &mut self,
        fence: &AttemptFence,
        input: OpaqueCheckpointInput,
        terminal: TerminalProposal,
    ) -> Result<FinalizedCheckpoint> {
        let request_id = input.request_id.clone();
        let prepared = checkpoints::prepare(fence, input, Some(&terminal))?;
        self.finish_prepared_checkpoint(fence, &request_id, prepared, terminal)
    }

    pub(crate) fn finish_prepared_checkpoint(
        &mut self,
        fence: &AttemptFence,
        request_id: &str,
        prepared: checkpoints::PreparedCheckpoint,
        terminal: TerminalProposal,
    ) -> Result<FinalizedCheckpoint> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // The mutating API never treats an inactive/expired fence as publication authority.
        checkpoint_cut::owner(&tx, fence)?;
        if checkpoint_read::by_request(&tx, &fence.session_id, request_id)?.is_some() {
            return Err(conflict(
                "checkpoint request exists; use read-only finalization recovery",
            ));
        }
        let checkpoint = checkpoints::publish(&tx, fence, prepared)?;
        let session = attempts::finish_in_transaction(
            &tx,
            fence,
            terminal.lifecycle,
            terminal.outcome,
            terminal.result.clone(),
            |tx, session| {
                let version = i64::try_from(session.version)
                    .map_err(|_| conflict("finalization version exceeds durable integer range"))?;
                let digest = receipt_digest(&checkpoint, session.version);
                let inserted = tx.execute(
                    "INSERT INTO context_finalizations(checkpoint_id,session_version,receipt_digest)
                     VALUES(?1,?2,?3)",
                    params![checkpoint.id.as_str(), version, digest.as_slice()],
                )?;
                if inserted != 1 {
                    return Err(conflict("finalization receipt insertion failed"));
                }
                // Includes hashing, terminal writes and receipt writes in the ownership window.
                checkpoint_cut::owner(tx, fence)
            },
        )?;
        tx.commit()?;
        Ok(FinalizedCheckpoint {
            checkpoint,
            terminal,
            session_version: session.version,
        })
    }

    /// Exact read-only recovery using the retained terminal-bound request digest.
    /// Works after owner deactivation and later attempts; never changes current session state.
    pub fn finished_checkpoint_request(
        &self,
        session_id: &str,
        request_id: &str,
        expected_digest: [u8; 32],
    ) -> Result<Option<FinalizedCheckpoint>> {
        checkpoint_codec::identifier(request_id)?;
        let tx = self.connection.unchecked_transaction()?;
        read_session(&tx, session_id)?;
        let Some(row) = checkpoint_read::by_request(&tx, session_id, request_id)? else {
            return Ok(None);
        };
        if row.request_digest != expected_digest {
            return Err(conflict("finalization request has different content"));
        }
        let metadata = row.metadata.clone();
        let checkpoint = row.private()?.reference;
        let terminal = checkpoint_codec::decode_terminal(&metadata)?;
        let receipt: Option<(i64, [u8; 32])> = tx.query_row(
            "SELECT session_version,receipt_digest FROM context_finalizations WHERE checkpoint_id=?1",
            [checkpoint.id.as_str()], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        let (version, digest) =
            receipt.ok_or_else(|| conflict("finalization receipt is missing"))?;
        let session_version =
            u64::try_from(version).map_err(|_| conflict("invalid finalization session version"))?;
        if receipt_digest(&checkpoint, session_version) != digest {
            return Err(conflict("finalization receipt integrity mismatch"));
        }
        tx.commit()?;
        Ok(Some(FinalizedCheckpoint {
            checkpoint,
            terminal,
            session_version,
        }))
    }
}

fn receipt_digest(checkpoint: &CheckpointRef, version: u64) -> [u8; 32] {
    checkpoint_cut::digest(
        b"foundry.checkpoint.finalization.v1",
        &[
            &checkpoint.request_digest,
            checkpoint.id.as_str().as_bytes(),
            &checkpoint.generation.to_be_bytes(),
            &checkpoint.committed_ms.to_be_bytes(),
            &version.to_be_bytes(),
        ],
    )
}
