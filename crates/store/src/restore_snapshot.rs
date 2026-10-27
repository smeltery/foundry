use crate::{
    CaptureSnapshot, PrivateCheckpoint, PrivateWitness, RestoreFence, Result, Store,
    capture_bounds, capture_snapshot, conflict, restore_read, witness_events, witness_facts,
};
mod bounds;
#[cfg(test)]
mod event_tests;

/// Owner-bound loading evidence, not native validation or permission to execute.
/// Deliberately has no Debug/Serialize implementation or public field access.
pub struct RestoreBaselineSnapshot {
    checkpoint: PrivateCheckpoint,
    witness: PrivateWitness,
    facts: CaptureSnapshot,
    events: Vec<witness_events::EvidenceEvent>,
}

impl RestoreBaselineSnapshot {
    pub fn checkpoint(&self) -> &PrivateCheckpoint {
        &self.checkpoint
    }
    pub fn witness(&self) -> &PrivateWitness {
        &self.witness
    }
    pub fn facts(&self) -> &CaptureSnapshot {
        &self.facts
    }
    pub fn events(&self) -> &[witness_events::EvidenceEvent] {
        &self.events
    }
}

impl Store {
    /// Read one complete selected baseline without admission, mutation or lease renewal.
    /// Retained facts and events are bounded; SQLite scan/cache work is not bounded.
    pub fn restore_baseline_snapshot(
        &self,
        fence: &RestoreFence,
    ) -> Result<RestoreBaselineSnapshot> {
        let tx = self.connection.unchecked_transaction()?;
        let owner = restore_read::load(&tx, &fence.id)?;
        owner.owner(fence, false)?;
        // Selection hashes durable facts; validate stored bounds before that scan.
        capture_bounds::preflight(&tx, &owner.session)?;
        let (checkpoint, witness) =
            restore_read::selected_baseline(&tx, &owner, owner.configuration)?;
        let selected = checkpoint.reference().clone();
        bounds::preflight(&tx, &owner.session, &selected)?;
        let witness = witness.ok_or_else(|| {
            foundry_contracts::ContractError::Unsupported("restore baseline has no witness".into())
        })?;
        let facts = capture_snapshot::snapshot_with_head(&tx, &owner.session, selected.clone())?;
        if facts.head.as_ref() != Some(&selected) || facts.cut != selected.cut {
            return Err(conflict("restore baseline selection changed"));
        }
        let events = witness_events::selected(&tx, &owner.session)?;
        if events
            .iter()
            .any(|event| event.sequence > selected.cut.event_seq)
        {
            return Err(conflict("restore evidence exceeds selected cut"));
        }
        witness_facts::verify_evidence(&tx, &owner.session, &witness.body, &facts, &events)?;
        let snapshot = RestoreBaselineSnapshot {
            witness: PrivateWitness::from_verified(selected, witness),
            checkpoint,
            facts,
            events,
        };
        restore_read::load(&tx, &fence.id)?.owner(fence, false)?;
        tx.commit()?;
        Ok(snapshot)
    }
}
