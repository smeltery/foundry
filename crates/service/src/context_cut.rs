//! Mapping of one durable store snapshot into trusted-owner runtime facts.
use crate::{Result, ServiceError};
use foundry_contracts::context::ContextDigest;
use foundry_runtime::coordinated::capture::{
    CaptureCut, CoreCutFacts, CoreCutIdentity, DurableCoverage, HeadIdentity, ProfileBinding,
};
use foundry_store::{AttemptFence, CaptureSnapshot, Store};

/// Retained for the linked validator; digest evidence is not native semantic proof.
pub(crate) struct CaptureEvidence {
    snapshot: CaptureSnapshot,
    session_id: String,
    attempt_id: String,
    request_id: String,
    cut_binding: ContextDigest,
}
impl CaptureEvidence {
    pub(crate) fn snapshot(&self) -> &CaptureSnapshot {
        &self.snapshot
    }
    pub(crate) fn cut_binding(&self) -> ContextDigest {
        self.cut_binding
    }
    pub(crate) fn check(&self, fence: &AttemptFence, cut: &CaptureCut) -> Result<()> {
        let identity = cut.identity();
        if fence.session_id != self.session_id
            || fence.attempt_id != self.attempt_id
            || identity.session_id != self.session_id
            || identity.attempt_id != self.attempt_id
            || identity.request_id != self.request_id
            || cut.binding_digest() != self.cut_binding
        {
            return Err(ServiceError::Coordination("capture cut binding mismatch"));
        }
        Ok(())
    }
}

pub(crate) fn prepare_cut(
    store: &Store,
    fence: &AttemptFence,
    request_id: String,
    profile: ProfileBinding,
) -> Result<(CaptureCut, CaptureEvidence)> {
    let snapshot = store.capture_snapshot(fence)?;
    let cut = CaptureCut::from_trusted_owner(
        CoreCutIdentity {
            session_id: fence.session_id.clone(),
            attempt_id: fence.attempt_id.clone(),
            request_id: request_id.clone(),
            expected_head: snapshot.head.as_ref().map(|head| HeadIdentity {
                checkpoint_id: head.id.as_str().into(),
                generation: head.generation,
            }),
        },
        CoreCutFacts {
            work_revision: snapshot.cut.work_revision,
            event_seq: snapshot.cut.event_seq,
            event_digest: snapshot.cut.event_digest,
            input_digest: snapshot.cut.input_digest,
            tool_digest: snapshot.cut.tool_digest,
        },
        profile,
        DurableCoverage {
            message_ids: snapshot.coverage.message_ids.clone(),
            tool_call_ids: snapshot.coverage.tool_call_ids.clone(),
        },
    )
    .map_err(|_| ServiceError::Coordination("invalid capture cut mapping"))?;
    let evidence = CaptureEvidence {
        snapshot,
        session_id: fence.session_id.clone(),
        attempt_id: fence.attempt_id.clone(),
        request_id,
        cut_binding: cut.binding_digest(),
    };
    Ok((cut, evidence))
}
