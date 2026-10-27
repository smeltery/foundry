use crate::{
    ContinueDecision, ContinueInput, ContinueMessageDecision, ContinueMessageInput,
    ContinueMessagePermit, ContinuePermit, ContinueReceipt, RestoreFence, Result, Store, admission,
    attempts, checkpoint_codec, conflict, now_ms, restore_message, restore_read, restores,
};
use rusqlite::{TransactionBehavior, params};

impl Store {
    /// Persist one-use authorization before returning a delivery permit. This does not deliver
    /// the command, prove execution, or authorize native context without trusted validation.
    pub fn authorize_continue(
        &mut self,
        fence: &RestoreFence,
        input: ContinueInput,
    ) -> Result<ContinueDecision> {
        match self.continue_transaction(fence, &input, None)? {
            Admission::Existing(receipt) => Ok(ContinueDecision::AlreadyAuthorized(receipt)),
            Admission::Granted(attempt, _, receipt) => Ok(ContinueDecision::Granted {
                permit: ContinuePermit(attempt),
                receipt,
            }),
        }
    }

    /// Atomically claim exactly the oldest pending message and authorize one delivery.
    /// A lost response is recoverable only as metadata, never as another permit.
    pub fn authorize_continue_message(
        &mut self,
        fence: &RestoreFence,
        input: ContinueMessageInput,
    ) -> Result<ContinueMessageDecision> {
        checkpoint_codec::identifier(&input.message_id)?;
        match self.continue_transaction(fence, &input.authorization, Some(&input))? {
            Admission::Existing(receipt) => Ok(ContinueMessageDecision::AlreadyAuthorized(receipt)),
            Admission::Granted(attempt, Some(message), receipt) => {
                Ok(ContinueMessageDecision::Granted {
                    permit: ContinueMessagePermit(attempt, message),
                    receipt,
                })
            }
            Admission::Granted(_, None, _) => {
                unreachable!("bound admission requires a claimed message")
            }
        }
    }

    fn continue_transaction(
        &mut self,
        fence: &RestoreFence,
        input: &ContinueInput,
        bound: Option<&ContinueMessageInput>,
    ) -> Result<Admission> {
        checkpoint_codec::identifier(&input.request_id)?;
        // Lost-response recovery after deactivation is read-only and never issues a token.
        let read = self.connection.unchecked_transaction()?;
        if let Some(receipt) = retry(&restore_read::load(&read, &fence.id)?, fence, input, bound)? {
            read.commit()?;
            return Ok(Admission::Existing(receipt));
        }
        read.commit()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = restore_read::load(&tx, &fence.id)?;
        if let Some(receipt) = retry(&row, fence, input, bound)? {
            tx.commit()?;
            return Ok(Admission::Existing(receipt));
        }
        row.owner(fence, false)?;
        if row.phase != "restored" {
            return Err(conflict("native restoration has not been confirmed"));
        }
        restore_read::selection(&tx, &row, input.configuration_digest)?;
        let binding = bound
            .map(|bound| restore_message::select(&tx, &row.session, bound))
            .transpose()?;
        let (attempt, session) = admission::admit(
            &tx,
            &row.session,
            row.version as u64,
            self.lease_duration,
            Some(&row.id),
        )?;
        let message = if let Some(binding) = &binding {
            let claimed = crate::messages::claim_next(&tx, &attempt)?
                .ok_or_else(|| conflict("bound message claim unavailable"))?;
            if claimed.id != binding.message_id || claimed.sequence != binding.sequence {
                return Err(conflict("bound message claim changed"));
            }
            restore_message::insert(&tx, &row, binding)?;
            Some(claimed)
        } else {
            None
        };
        let revision: i64 = tx.query_row(
            "SELECT revision FROM session_work WHERE session_id=?1",
            [&row.session],
            |r| r.get(0),
        )?;
        let version = i64::try_from(session.version)
            .map_err(|_| conflict("restore admission version overflow"))?;
        let receipt = ContinueReceipt {
            restore_id: row.id.clone(),
            attempt_id: attempt.attempt_id.clone(),
            request_id: input.request_id.clone(),
            delivery_digest: input.delivery_digest,
            checkpoint_id: crate::ContextCheckpointId::parse(&row.checkpoint)?,
            session_version: session.version,
            admitted_work_revision: revision as u64,
            message: binding,
        };
        let digest = restore_message::request_digest(&row, input, receipt.message.as_ref());
        let receipt_digest = restore_read::receipt_digest(&receipt, digest);
        // Last write transfers exclusion only if the loading owner is still live/uncancelled.
        restores::changed(tx.execute(
            "UPDATE context_restores SET phase='continuing',continue_request_id=?1,continue_digest=?2,
             delivery_digest=?3,attempt_id=?4,admitted_session_version=?5,admitted_work_revision=?6,receipt_digest=?7
             WHERE id=?8 AND session_id=?9 AND token=?10 AND phase='restored' AND cancel IS NULL AND expires_ms>?11",
            params![input.request_id,digest.as_slice(),input.delivery_digest.as_slice(),attempt.attempt_id,
                version,revision,receipt_digest.as_slice(),fence.id,fence.session,fence.token,now_ms()?],
        )?)?;
        attempts::check_fence(&tx, &attempt)?;
        let cancelled: bool = tx.query_row(
            "SELECT cancel IS NOT NULL FROM attempts WHERE id=?1",
            [&attempt.attempt_id],
            |r| r.get(0),
        )?;
        if cancelled {
            return Err(conflict("admitted attempt cancelled"));
        }
        tx.commit()?;
        Ok(Admission::Granted(attempt, message, receipt))
    }
}
enum Admission {
    Existing(ContinueReceipt),
    Granted(
        crate::AttemptFence,
        Option<foundry_contracts::Message>,
        ContinueReceipt,
    ),
}
fn retry(
    row: &restore_read::Row,
    fence: &RestoreFence,
    input: &ContinueInput,
    bound: Option<&ContinueMessageInput>,
) -> Result<Option<ContinueReceipt>> {
    row.credential(fence)?;
    let Some(receipt) = row.receipt()? else {
        return Ok(None);
    };
    match (bound, receipt.message.as_ref()) {
        (None, None) => {}
        (Some(input), Some(message))
            if input.message_id == message.message_id
                && input.expected_text_digest == message.text_digest => {}
        _ => return Err(conflict("Continue authorization mode or message differs")),
    }
    if row.continue_digest
        != Some(restore_message::request_digest(
            row,
            input,
            receipt.message.as_ref(),
        ))
    {
        return Err(conflict(
            "Continue already authorized with different content",
        ));
    }
    Ok(Some(receipt))
}
