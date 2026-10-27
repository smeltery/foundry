use crate::{
    AttemptFence, OpaqueCheckpointInput, Result, TerminalProposal, checkpoint_codec,
    checkpoint_cut::digest, checkpoint_read::Stored, conflict, witness::WitnessBodyV1,
};
use foundry_contracts::{Lifecycle, Outcome};
use sha2::{Digest, Sha256};
pub(crate) fn check_input(
    body: &WitnessBodyV1,
    fence: &AttemptFence,
    input: &OpaqueCheckpointInput,
    terminal: &TerminalProposal,
) -> Result<()> {
    let h = &body.header;
    if h.session_id != fence.session_id
        || h.source_attempt_id != fence.attempt_id
        || h.source_request_id != input.request_id
        || h.stored_format != input.format
        || h.configuration_digest != input.configuration_digest
        || h.cut != input.cut
        || h.payload_length != input.payload.len() as u64
        || h.payload_digest != <[u8; 32]>::from(Sha256::digest(&input.payload))
        || h.parent.as_ref().map(|p| p.checkpoint_id.as_str())
            != input.expected_head.as_ref().map(|p| p.as_str())
        || h.terminal_digest != terminal_digest(terminal)?
    {
        return Err(conflict("witness header differs from checkpoint input"));
    }
    Ok(())
}
pub(crate) fn check_row(row: &Stored) -> Result<()> {
    match (row.request_format, &row.witness) {
        (1, None) => Ok(()),
        (2, Some(w)) => {
            let (format, cut, _, _) = checkpoint_codec::decode(&row.metadata, &row.coverage)?;
            let terminal = checkpoint_codec::decode_terminal(&row.metadata)?;
            let h = &w.body.header;
            if h.session_id != row.session
                || h.source_attempt_id != row.attempt
                || h.source_request_id != row.request
                || h.stored_format != format
                || h.cut != cut
                || h.configuration_digest != row.configuration_digest
                || h.payload_digest != row.payload_digest
                || h.payload_length != row.payload.len() as u64
                || h.parent.as_ref().map(|p| p.checkpoint_id.as_str()) != row.parent.as_deref()
                || h.terminal_digest != terminal_digest(&terminal)?
            {
                return Err(conflict("stored witness header mismatch"));
            }
            Ok(())
        }
        _ => Err(conflict("invalid checkpoint request format")),
    }
}
pub(crate) fn terminal_digest(terminal: &TerminalProposal) -> Result<[u8; 32]> {
    if terminal.lifecycle != Lifecycle::Exit {
        return Err(conflict("witness requires an exit terminal"));
    }
    let outcome = match terminal.outcome {
        Some(Outcome::Success) => b"success".as_slice(),
        Some(Outcome::Failure) => b"failure".as_slice(),
        Some(Outcome::ActionRequired) => b"action_required".as_slice(),
        None => return Err(conflict("witness terminal outcome missing")),
    };
    let result = terminal
        .result
        .as_ref()
        .ok_or_else(|| conflict("witness terminal result missing"))?;
    Ok(digest(
        b"foundry.capture.terminal.v1",
        &[b"exit", outcome, result.as_bytes()],
    ))
}
