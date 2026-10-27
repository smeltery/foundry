#[path = "../support_witness/mod.rs"]
pub mod witness;
use foundry_store::*;
use sha2::{Digest, Sha256};

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub store: Store,
    pub fence: AttemptFence,
    pub input: ContinueAcceptanceInput,
}
impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("db")).unwrap();
        let session = witness::checkpoint::session(&mut store);
        let fence = store.begin_attempt(&session, 0).unwrap();
        let baseline = store
            .finish_with_witnessed_checkpoint(
                &fence,
                witness::input(&store, &fence, "root"),
                witness::terminal(),
            )
            .unwrap()
            .checkpoint;
        let owner = match store
            .begin_restore(
                &session,
                BeginRestoreInput {
                    request_id: "restore".into(),
                    checkpoint_id: baseline.id.clone(),
                    expected_session_version: store.get(&session).unwrap().version,
                    configuration_digest: [7; 32],
                },
            )
            .unwrap()
        {
            BeginRestoreResult::Started { fence, .. } => fence,
            _ => panic!("new restore"),
        };
        store.confirm_restored(&owner, [7; 32]).unwrap();
        let message = store
            .enqueue_message(&session, "instruction", "next instruction")
            .unwrap();
        let intent = RestoreContinueIntentV1 {
            session_id: session,
            restore_id: owner.restore_id().into(),
            load_id: uuid::Uuid::new_v4().to_string(),
            continuation_id: "continue".into(),
            baseline_checkpoint_id: baseline.id.clone(),
            baseline_record_digest: baseline.record_digest,
            baseline_witness_digest: store
                .read_checkpoint_witness(&baseline.session_id, &baseline.id)
                .unwrap()
                .digest(),
            // Deliberately distinct from capture validation: this is trusted loading evidence.
            baseline_validation_binding: [93; 32],
            configuration_digest: [7; 32],
            message_id: message.id,
            message_sequence: message.sequence,
            message_text_digest: Sha256::digest(message.text.as_bytes()).into(),
            message_text_bytes: message.text.len() as u64,
            mode: RestoreContinueMode::QueuedInstruction,
            codec: RestoreContinueCodec::RestoreContinueV2,
            tail_receipt_digest: [42; 32],
        };
        let (fence, authorization) = match store
            .authorize_continue_message(
                &owner,
                ContinueMessageInput {
                    authorization: ContinueInput {
                        request_id: intent.continuation_id.clone(),
                        configuration_digest: [7; 32],
                        delivery_digest: intent.delivery_digest().unwrap(),
                    },
                    message_id: intent.message_id.clone(),
                    expected_text_digest: intent.message_text_digest,
                },
            )
            .unwrap()
        {
            ContinueMessageDecision::Granted { permit, receipt } => {
                (permit.into_delivery().0, receipt)
            }
            _ => panic!("new admission"),
        };
        let mut input = ContinueAcceptanceInput {
            authorization,
            intent,
            authorization_binding: [0; 32],
            observed_tail_receipt_digest: [42; 32],
        };
        input.authorization_binding = input.computed_authorization_binding().unwrap();
        Self {
            dir,
            store,
            fence,
            input,
        }
    }
    pub fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.dir.path().join("db")).unwrap()
    }
    pub fn accept(&mut self) -> foundry_store::Result<ContinueAcceptanceRef> {
        self.store
            .accept_restore_continuation(&self.fence, self.input.clone())
    }
    pub fn lookup(&self) -> foundry_store::Result<Option<ContinueAcceptanceRef>> {
        self.store.continuation_acceptance(
            &self.fence.session_id,
            &self.input.intent.restore_id,
            self.input.request_digest().unwrap(),
        )
    }
}
