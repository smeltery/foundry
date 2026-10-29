use super::validation::{ValidatedLoad, digest};
use super::*;
use foundry_contracts::{Message, MessageState, context_v2::ContinuationIdentity};
use foundry_runtime::coordinated::loading::continuation::*;
use foundry_store::{
    AttemptFence, ContinueAcceptanceInput, ContinueInput, ContinueMessageDecision,
    ContinueMessageInput, ContinueReceipt, RestoreContinueCodec, RestoreContinueIntentV1,
    RestoreContinueMode, RestoreFence, Store,
};

/// Exact queued instruction selected by the trusted application; Store checks it again.
pub struct ContinueSelection {
    pub request_id: String,
    pub message_id: String,
    pub sequence: u64,
    pub text_digest: ContextDigest,
}
pub(super) struct Link {
    requests: Receiver<ContinuationRequest>,
    replies: SyncSender<ContinuationReply>,
    selection: ContinueSelection,
    pub grant: Option<OwnedGrant>,
}
pub(super) struct OwnedGrant {
    pub fence: AttemptFence,
    message: Message,
    receipt: ContinueReceipt,
    intent: RestoreContinueIntentV1,
    job: ContinuationJob,
    identity: Option<ContinuationIdentity>,
    accepted: bool,
    pub acceptance: Option<foundry_store::ContinueAcceptanceRef>,
}
impl Link {
    pub fn new(selection: ContinueSelection) -> Result<(Self, ContinuationOwner)> {
        if selection.sequence == 0
            || selection.request_id.is_empty()
            || selection.message_id.is_empty()
        {
            return Err(ServiceError::Invalid("invalid continuation selection"));
        }
        let (request, requests) = mpsc::sync_channel(1);
        let (replies, reply) = mpsc::sync_channel(1);
        Ok((
            Self {
                requests,
                replies,
                selection,
                grant: None,
            },
            ContinuationOwner {
                requests: request,
                replies: reply,
            },
        ))
    }
    pub fn poll(
        &mut self,
        store: &mut Store,
        fence: &RestoreFence,
        snapshot: &RestoreBaselineSnapshot,
        validated: &ValidatedLoad,
        cancelled: impl Fn() -> bool,
    ) -> Result<()> {
        let request = match self.requests.try_recv() {
            Ok(request) => request,
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(_) if self.grant.as_ref().is_some_and(|grant| grant.accepted) => return Ok(()),
            Err(_) => {
                return Err(ServiceError::Coordination(
                    "continuation supervisor disconnected",
                ));
            }
        };
        if cancelled() {
            return Err(ServiceError::Coordination("continuation owner stopped"));
        }
        let job = match &request {
            ContinuationRequest::Authorize { job, .. }
            | ContinuationRequest::Accept { job, .. } => job.clone(),
        };
        let result = match request {
            ContinuationRequest::Authorize {
                job,
                binding,
                tail_receipt_digest,
            } => {
                if self.grant.is_some()
                    || binding.observation != validated.binding
                    || binding.instance_id != validated.instance
                    || binding.effective_configuration_digest
                        != validated.effective_configuration_digest
                    || binding.baseline.binding_digest().ok() != Some(validated.origin_digest)
                {
                    Err(ServiceError::Coordination(
                        "continuation validation mismatch",
                    ))
                } else {
                    self.authorize(
                        store,
                        fence,
                        snapshot,
                        validated,
                        (job, tail_receipt_digest),
                        &cancelled,
                    )
                }
            }
            ContinuationRequest::Accept {
                job,
                scope,
                identity,
                receipt,
            } => self.accept(store, job, scope, identity, receipt, &cancelled),
        };
        match result {
            Ok(reply) => self
                .replies
                .try_send(reply)
                .map_err(|_| ServiceError::Coordination("continuation reply lost")),
            Err(error) => {
                let _ = self.replies.try_send(ContinuationReply::Rejected { job });
                Err(error)
            }
        }
    }
    fn authorize(
        &mut self,
        store: &mut Store,
        fence: &RestoreFence,
        snapshot: &RestoreBaselineSnapshot,
        validated: &ValidatedLoad,
        request: (ContinuationJob, ContextDigest),
        cancelled: &impl Fn() -> bool,
    ) -> Result<ContinuationReply> {
        let (job, tail) = request;
        let fresh = store.restore_baseline_snapshot(fence)?;
        if fresh.checkpoint().reference() != snapshot.checkpoint().reference()
            || fresh.witness().digest() != snapshot.witness().digest()
        {
            return Err(ServiceError::Coordination("continuation baseline changed"));
        }
        let selected = &self.selection;
        let message = store
            .messages(
                &snapshot.checkpoint().reference().session_id,
                selected.sequence - 1,
                1,
            )?
            .into_iter()
            .next()
            .ok_or(ServiceError::Coordination(
                "continuation instruction missing",
            ))?;
        if message.id != selected.message_id
            || message.sequence != selected.sequence
            || message.state != MessageState::Pending
            || ContextDigest::of(message.text.as_bytes()) != selected.text_digest
        {
            return Err(ServiceError::Coordination(
                "continuation instruction changed",
            ));
        }
        let head = snapshot.checkpoint().reference();
        let intent = RestoreContinueIntentV1 {
            session_id: head.session_id.clone(),
            restore_id: fence.restore_id().into(),
            load_id: validated.binding.scope.load_id.as_str().into(),
            continuation_id: selected.request_id.clone(),
            baseline_checkpoint_id: head.id.clone(),
            baseline_record_digest: head.record_digest,
            baseline_witness_digest: snapshot.witness().digest(),
            baseline_validation_binding: validated.validation_digest,
            configuration_digest: head.configuration_digest,
            message_id: message.id.clone(),
            message_sequence: message.sequence,
            message_text_digest: raw(selected.text_digest),
            message_text_bytes: message.text.len() as u64,
            mode: RestoreContinueMode::QueuedInstruction,
            codec: RestoreContinueCodec::RestoreContinueV2,
            tail_receipt_digest: raw(tail),
        };
        let input = ContinueMessageInput {
            authorization: ContinueInput {
                request_id: selected.request_id.clone(),
                configuration_digest: head.configuration_digest,
                delivery_digest: intent.delivery_digest()?,
            },
            message_id: message.id,
            expected_text_digest: raw(selected.text_digest),
        };
        if cancelled() {
            return Err(ServiceError::Coordination(
                "continuation stopped before grant",
            ));
        }
        let ContinueMessageDecision::Granted { permit, receipt } =
            store.authorize_continue_message(fence, input)?
        else {
            return Err(ServiceError::Coordination(
                "continuation retry is metadata only",
            ));
        };
        let (attempt, message) = permit.into_delivery();
        // First action after consuming the permit: retain authority before fallible work.
        self.grant = Some(OwnedGrant {
            fence: attempt,
            message,
            receipt,
            intent,
            job: job.clone(),
            identity: None,
            accepted: false,
            acceptance: None,
        });
        let grant = self.grant.as_mut().expect("grant installed");
        if cancelled() {
            return Err(ServiceError::Coordination(
                "continuation stopped after grant",
            ));
        }
        let mut acceptance = grant.acceptance();
        acceptance.authorization_binding = acceptance.computed_authorization_binding()?;
        let id = |value: &str| {
            ContextId::new(value)
                .map_err(|_| ServiceError::Coordination("invalid continuation identity"))
        };
        let identity = ContinuationIdentity {
            attempt_id: id(&grant.fence.attempt_id)?,
            continuation_id: id(&grant.intent.continuation_id)?,
            input_id: id(&grant.message.id)?,
            input_sequence: grant.message.sequence,
            input_digest: digest(grant.intent.message_text_digest),
            input_bytes: grant.intent.message_text_bytes,
            authorization_binding: digest(acceptance.authorization_binding),
        };
        grant.identity = Some(identity.clone());
        Ok(ContinuationReply::Granted {
            job,
            identity,
            text: grant.message.text.clone(),
        })
    }
    fn accept(
        &mut self,
        store: &mut Store,
        job: ContinuationJob,
        scope: LoadingScope,
        identity: ContinuationIdentity,
        receipt: ContextReceipt,
        cancelled: &impl Fn() -> bool,
    ) -> Result<ContinuationReply> {
        let grant = self
            .grant
            .as_mut()
            .ok_or(ServiceError::Coordination("continuation grant absent"))?;
        if grant.accepted
            || grant.job != job
            || grant.identity.as_ref() != Some(&identity)
            || scope.restore_id.as_str() != grant.intent.restore_id
            || scope.load_id.as_str() != grant.intent.load_id
            || raw(ContextDigest::of(receipt.as_str().as_bytes()))
                != grant.intent.tail_receipt_digest
        {
            return Err(ServiceError::Coordination(
                "continuation acceptance mismatch",
            ));
        }
        let mut input = grant.acceptance();
        input.authorization_binding = input.computed_authorization_binding()?;
        grant.acceptance = Some(store.accept_restore_continuation(&grant.fence, input)?);
        grant.accepted = true;
        if cancelled() || store.cancellation(&grant.fence)?.is_some() {
            return Err(ServiceError::Coordination(
                "continuation activation cancelled",
            ));
        }
        // Read validates the still-live attempt credential without resetting watchdog.
        store.heartbeat(&grant.fence)?;
        if cancelled() {
            return Err(ServiceError::Coordination(
                "continuation activation expired",
            ));
        }
        Ok(ContinuationReply::Activated { job })
    }
}
impl OwnedGrant {
    fn acceptance(&self) -> ContinueAcceptanceInput {
        ContinueAcceptanceInput {
            authorization: self.receipt.clone(),
            intent: self.intent.clone(),
            authorization_binding: [0; 32],
            observed_tail_receipt_digest: self.intent.tail_receipt_digest,
        }
    }
}
pub(super) fn raw(value: ContextDigest) -> [u8; 32] {
    let hex = value.to_hex();
    let mut result = [0; 32];
    for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
        let digit = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
        result[index] = digit(pair[0]) * 16 + digit(pair[1]);
    }
    result
}
