use crate::{
    ServiceError,
    tool_worker::{self, WorkerControl},
};
use foundry_contracts::{CancelMode, Session};
use foundry_runtime::{
    AdapterMessage, AdapterSpec, RunResult, RuntimeError,
    coordinated::{
        PERSISTENCE_CAPACITY, RuntimePersistence, TrustedToolPolicy, run_adapter_coordinated,
    },
};
use foundry_store::{AttemptFence, Store};
use std::{sync::mpsc, thread};

pub(crate) struct CoordinatedRun<'a> {
    pub store: &'a mut Store,
    pub fence: &'a AttemptFence,
    pub session: &'a Session,
    pub spec: &'a AdapterSpec,
    pub cancellation: &'a mpsc::Receiver<CancelMode>,
    pub messages: &'a mpsc::Receiver<AdapterMessage>,
    pub control: WorkerControl,
}
impl CoordinatedRun<'_> {
    pub fn run(
        self,
        policy: TrustedToolPolicy,
        stopped: impl FnOnce(),
    ) -> (
        std::result::Result<RunResult, RuntimeError>,
        Option<ServiceError>,
    ) {
        thread::scope(|scope| {
            let (requests_tx, requests_rx) = mpsc::sync_channel(PERSISTENCE_CAPACITY);
            let (replies_tx, replies_rx) = mpsc::sync_channel(PERSISTENCE_CAPACITY);
            let worker = thread::Builder::new()
                .name("foundry-persistence".into())
                .spawn_scoped(scope, move || {
                    tool_worker::run(
                        self.store,
                        self.fence,
                        requests_rx,
                        replies_tx,
                        self.control,
                    )
                });
            let worker = match worker {
                Ok(worker) => worker,
                Err(_) => {
                    stopped();
                    return (
                        Err(RuntimeError::Invalid("cannot start persistence worker")),
                        Some(ServiceError::Coordination(
                            "cannot start persistence worker",
                        )),
                    );
                }
            };
            let result = run_adapter_coordinated(
                self.spec,
                self.session,
                &self.fence.attempt_id,
                self.cancellation,
                self.messages,
                RuntimePersistence {
                    requests: requests_tx,
                    replies: replies_rx,
                },
                Some(policy),
            );
            // Runtime has cleaned both process owners before a SQLite join may block.
            stopped();
            let failure = match worker.join() {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(_) => Some(ServiceError::Coordination("persistence worker panicked")),
            };
            (result, failure)
        })
    }
}
