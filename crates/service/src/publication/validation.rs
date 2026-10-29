use super::*;
use crate::native_validation::NativeValidationFailure;
use crate::restoration_owner::validation::{digest, hash_fields};
use foundry_runtime::coordinated::capture::CaptureValidationJob;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread::{self, JoinHandle},
};

pub(super) struct Task {
    job: Option<Arc<NativePublicationJob>>,
    correlation: CaptureCorrelation,
    result:
        mpsc::Receiver<std::result::Result<NativePublicationComparison, NativeValidationFailure>>,
    worker: Option<JoinHandle<()>>,
}
pub(super) type Completed = (
    CaptureCorrelation,
    NativePublicationJob,
    NativePublicationComparison,
);
impl Task {
    pub fn start(
        configuration: &PublicationConfiguration,
        correlation: CaptureCorrelation,
        runtime: Box<CaptureValidationJob>,
        snapshot: Arc<foundry_store::CaptureSnapshot>,
        baseline: Option<Arc<RestoreBaselineSnapshot>>,
        acceptance: Option<ContinueAcceptanceRef>,
    ) -> Result<Self> {
        if runtime.candidate().payload().len() > 1048576 {
            return Err(ServiceError::Invalid(
                "publication candidate exceeds profile bound",
            ));
        }
        let baseline_digest = baseline
            .as_ref()
            .map(|b| b.witness().digest())
            .unwrap_or([0; 32]);
        let acceptance_digest = acceptance
            .as_ref()
            .map(|a| a.record_digest)
            .unwrap_or([0; 32]);
        let input = hash_fields(
            b"foundry.capture.native-validation.v1",
            &[
                runtime.binding_digest().to_hex().as_bytes(),
                configuration.validator.identity.to_hex().as_bytes(),
                &configuration.policy,
                &configuration.associations,
                &baseline_digest,
                &acceptance_digest,
            ],
        );
        let job = Arc::new(NativePublicationJob {
            runtime,
            snapshot,
            baseline,
            acceptance,
            registration: configuration.validator.identity,
            input_digest: digest(input),
            policy: Arc::from(configuration.policy.clone()),
            associations: Arc::from(configuration.associations.clone()),
            deadline: Instant::now() + configuration.validator.timeout,
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        let work = Arc::clone(&job);
        let comparator = Arc::clone(&configuration.validator.comparator);
        let (send, result) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("foundry-publication-comparison".into())
            .spawn(move || {
                let result = comparator.compare(&work);
                let _ = send.try_send(result);
            })
            .map_err(|_| ServiceError::Coordination("publication worker unavailable"))?;
        Ok(Self {
            job: Some(job),
            correlation,
            result,
            worker: Some(worker),
        })
    }
    pub fn poll(&mut self) -> Result<Option<Completed>> {
        let job = self.job.as_ref().ok_or(ServiceError::Coordination(
            "publication result already consumed",
        ))?;
        if job.cancelled() {
            return Err(ServiceError::Coordination(
                "publication comparison deadline",
            ));
        }
        let compared = match self.result.try_recv() {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => return Err(ServiceError::Coordination("native publication rejected")),
            Err(mpsc::TryRecvError::Empty) => return Ok(None),
            Err(_) => return Err(ServiceError::Coordination("publication worker failed")),
        };
        if self.worker.take().is_some_and(|w| w.join().is_err())
            || job.cancelled()
            || compared.input_digest != job.input_digest
            || compared.registration_digest != job.registration
            || compared.candidate_digest != ContextDigest::of(job.runtime.candidate().payload())
        {
            return Err(ServiceError::Coordination(
                "publication result correlation mismatch",
            ));
        }
        let job = Arc::try_unwrap(self.job.take().expect("job installed"))
            .map_err(|_| ServiceError::Coordination("publication input ownership retained"))?;
        Ok(Some((self.correlation.clone(), job, compared)))
    }
    pub fn cancel(&self) {
        if let Some(job) = &self.job {
            job.cancelled.store(true, Ordering::Release);
        }
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
