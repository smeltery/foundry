use super::*;
use crate::native_validation::*;
use foundry_contracts::context_observation::{ObservationBinding, ObservationFrame};
use sha2::{Digest, Sha256};

pub(super) struct NativeState {
    registration: NativeValidatorRegistration,
    expected: Arc<NativeExpectations>,
    pub binding: ObservationBinding,
    task: Option<Task>,
    pub validated: Option<ValidatedLoad>,
    started: bool,
}
pub(super) struct ValidatedLoad {
    pub binding: ObservationBinding,
    pub instance: ContextReceipt,
    pub validation_digest: [u8; 32],
    pub origin_digest: ContextDigest,
    pub effective_configuration_digest: ContextDigest,
}
struct Task {
    job: Arc<NativeValidationJob>,
    result: Receiver<std::result::Result<NativeComparison, NativeValidationFailure>>,
    worker: Option<JoinHandle<()>>,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.job.cancelled.store(true, Ordering::Release);
        // Registration promises bounded pure work or checked host process cleanup.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
pub(crate) fn hash_fields(domain: &[u8], fields: &[&[u8]]) -> [u8; 32] {
    let mut hash = Sha256::new();
    for bytes in std::iter::once(domain).chain(fields.iter().copied()) {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    hash.finalize().into()
}
pub(crate) fn digest(bytes: [u8; 32]) -> ContextDigest {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    ContextDigest::parse(&hex).expect("digest encoding")
}
impl NativeState {
    pub fn prepare(
        registration: NativeValidatorRegistration,
        expected: NativeExpectations,
        scope: LoadingScope,
        begin: ContextDigest,
        snapshot: &RestoreBaselineSnapshot,
    ) -> Result<Self> {
        if expected.policy.len() > 65536
            || expected.associations.len() > 262144
            || snapshot.checkpoint().payload().len() > 1048576
            || !expected.origin.is_restored()
            || registration.timeout.is_zero()
            || Instant::now().checked_add(registration.timeout).is_none()
        {
            return Err(ServiceError::Invalid(
                "native validator selection or bounds",
            ));
        }
        Ok(Self {
            binding: ObservationBinding {
                scope,
                job_id: id()?,
                begin_binding: begin,
                profile_digest: expected.profile_digest,
            },
            registration,
            expected: Arc::new(expected),
            task: None,
            validated: None,
            started: false,
        })
    }
    pub fn start(
        &mut self,
        snapshot: Arc<RestoreBaselineSnapshot>,
        frame: ObservationFrame,
        instance: ContextReceipt,
    ) -> Result<()> {
        if self.started
            || frame.binding() != &self.binding
            || frame.instance_id() != Some(&instance)
            || frame.failure().is_some()
        {
            return Err(ServiceError::Coordination("native observation mismatch"));
        }
        let observation = frame
            .payload()
            .map_err(|_| ServiceError::Coordination("invalid native observation"))?
            .ok_or(ServiceError::Coordination("native observation missing"))?;
        let origin = self
            .expected
            .origin
            .encode()
            .map_err(|_| ServiceError::Coordination("native origin invalid"))?;
        let head = snapshot.checkpoint().reference();
        let input = super::binding::InputCommitment {
            binding: &self.binding,
            registration: self.registration.identity,
            record: &head.record_digest,
            witness: &snapshot.witness().digest(),
            stored_configuration: &head.configuration_digest,
            effective_configuration: self.expected.configuration_digest,
            payload: &head.payload_digest,
            policy: &self.expected.policy,
            associations: &self.expected.associations,
            origin: &origin,
            challenge: frame.challenge(),
            instance: &instance,
            observation: observation.digest(),
        }
        .digest();
        let job = Arc::new(NativeValidationJob {
            binding: self.binding.clone(),
            registration: self.registration.identity,
            snapshot,
            expected: Arc::clone(&self.expected),
            challenge: frame.challenge().clone(),
            instance,
            observation,
            input_digest: digest(input),
            deadline: Instant::now() + self.registration.timeout,
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        let (send, result) = mpsc::sync_channel(1);
        let work = Arc::clone(&job);
        let comparator = Arc::clone(&self.registration.comparator);
        let worker = thread::Builder::new()
            .name("foundry-native-comparison".into())
            .spawn(move || {
                let result = comparator.compare(&work);
                let _ = send.try_send(result);
            })
            .map_err(|_| ServiceError::Coordination("native worker unavailable"))?;
        self.task = Some(Task {
            job,
            result,
            worker: Some(worker),
        });
        self.started = true;
        Ok(())
    }
    pub fn poll(&mut self) -> Result<Option<ValidatedLoad>> {
        let Some(task) = self.task.as_mut() else {
            return Ok(None);
        };
        if task.job.cancelled() {
            return Err(ServiceError::Coordination("native validation deadline"));
        }
        let compared = match task.result.try_recv() {
            Ok(Ok(value)) => value,
            Ok(Err(_)) => return Err(ServiceError::Coordination("native comparison rejected")),
            Err(mpsc::TryRecvError::Empty) => return Ok(None),
            Err(_) => {
                return Err(ServiceError::Coordination(
                    "native comparison worker failed",
                ));
            }
        };
        if task
            .worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            return Err(ServiceError::Coordination(
                "native comparison worker panicked",
            ));
        }
        let job = &task.job;
        if job.cancelled()
            || compared.job_id != job.binding.job_id
            || compared.input_digest != job.input_digest
            || compared.registration_digest != job.registration
            || compared.instance_id != job.instance
            || compared.observation_digest != job.observation.digest()
            || compared.profile_digest != job.binding.profile_digest
            || compared.configuration_digest != job.expected.configuration_digest
        {
            return Err(ServiceError::Coordination(
                "native comparison correlation failed",
            ));
        }
        let validation_digest = hash_fields(
            b"foundry.restore.validation-result.v1",
            &[
                job.input_digest.to_hex().as_bytes(),
                compared.graph_digest.to_hex().as_bytes(),
                compared.mappings_digest.to_hex().as_bytes(),
            ],
        );
        let validated = ValidatedLoad {
            binding: job.binding.clone(),
            instance: job.instance.clone(),
            validation_digest,
            effective_configuration_digest: job.expected.configuration_digest,
            origin_digest: job
                .expected
                .origin
                .binding_digest()
                .map_err(|_| ServiceError::Coordination("native origin invalid"))?,
        };
        self.task = None;
        Ok(Some(validated))
    }
    pub fn cancel(&self) {
        if let Some(task) = &self.task {
            task.job.cancelled.store(true, Ordering::Release);
        }
    }
}

pub(crate) fn raw_digest(value: ContextDigest) -> [u8; 32] {
    super::continuation::raw(value)
}
