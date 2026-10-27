use foundry_contracts::{Lifecycle, Outcome};
use foundry_store::{
    BeginRestoreInput, BeginRestoreResult, CheckpointRef, ContinueInput, RestoreFence, Store,
    TerminalProposal,
};
#[path = "../support_checkpoint/mod.rs"]
pub mod checkpoint;

pub fn capture(store: &mut Store) -> (String, CheckpointRef) {
    let session = checkpoint::session(store);
    let fence = store.begin_attempt(&session, 0).unwrap();
    let input = checkpoint::input(store, &fence, "capture");
    let finished = store
        .finish_with_opaque_checkpoint(
            &fence,
            input,
            TerminalProposal {
                lifecycle: Lifecycle::Exit,
                outcome: Some(Outcome::Success),
                result: Some("prior terminal result".into()),
            },
        )
        .unwrap();
    (session, finished.checkpoint)
}
pub fn request(
    store: &Store,
    session: &str,
    checkpoint: &CheckpointRef,
    id: &str,
) -> BeginRestoreInput {
    BeginRestoreInput {
        request_id: id.into(),
        checkpoint_id: checkpoint.id.clone(),
        expected_session_version: store.get(session).unwrap().version,
        configuration_digest: [7; 32],
    }
}
pub fn begin(
    store: &mut Store,
    session: &str,
    checkpoint: &CheckpointRef,
    id: &str,
) -> RestoreFence {
    match store
        .begin_restore(session, request(store, session, checkpoint, id))
        .unwrap()
    {
        BeginRestoreResult::Started { fence, .. } => fence,
        _ => panic!("expected fresh restore owner"),
    }
}
pub fn continuation(id: &str) -> ContinueInput {
    ContinueInput {
        request_id: id.into(),
        configuration_digest: [7; 32],
        delivery_digest: [9; 32],
    }
}
