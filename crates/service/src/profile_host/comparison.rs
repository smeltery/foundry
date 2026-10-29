use super::{Blob, Configured, Error, Result, client::Operation, facts, projection};
use crate::{native_publication::*, native_validation::*};
use foundry_contracts::context::{ContextDigest, ContextId, ContextReceipt};
use foundry_runtime::coordinated::capture::OwnerValidationAttestation;
use foundry_store::witness::WitnessBodyV1;
use serde::Deserialize;
use serde_json::json;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observed {
    job_id: ContextReceipt,
    input_digest: ContextDigest,
    instance_id: ContextReceipt,
    observation_digest: ContextDigest,
    profile_digest: ContextDigest,
    configuration_digest: ContextDigest,
    graph_digest: ContextDigest,
    mappings_digest: ContextDigest,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Attestation {
    validator_id: ContextId,
    validator_version: ContextId,
    completed_turn_id: ContextId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Published {
    input_digest: ContextDigest,
    runtime_binding: ContextDigest,
    candidate_digest: ContextDigest,
    attestation: Attestation,
    witness: Blob,
}
impl NativeComparator for Configured {
    fn compare(&self, job: &NativeValidationJob) -> Result<NativeComparison> {
        if job.registration_digest() != self.registration_digest() {
            return Err(Error::Policy);
        }
        let expected = job.expectations();
        let input = json!({"job_id":job.binding().job_id,"input_digest":job.input_digest(),"profile_digest":expected.profile_digest,"configuration_digest":expected.configuration_digest,"binding":job.binding(),"instance_id":job.instance_id(),"policy":Blob::new(&expected.policy,65536)?,"associations":Blob::new(&expected.associations,262144)?,"baseline":facts::baseline(job.snapshot())?,"observation":Blob::new(job.observation(),1048576)?});
        let value: Observed =
            self.registration
                .call(Operation::Observe, &input, job.deadline(), || {
                    job.cancelled()
                })?;
        if value.job_id != job.binding().job_id
            || value.input_digest != job.input_digest()
            || &value.instance_id != job.instance_id()
            || value.observation_digest != ContextDigest::of(job.observation())
            || value.profile_digest != expected.profile_digest
            || value.configuration_digest != expected.configuration_digest
        {
            return Err(Error::Mapping);
        }
        Ok(NativeComparison {
            job_id: value.job_id,
            input_digest: value.input_digest,
            registration_digest: job.registration_digest(),
            instance_id: value.instance_id,
            observation_digest: value.observation_digest,
            profile_digest: value.profile_digest,
            configuration_digest: value.configuration_digest,
            graph_digest: value.graph_digest,
            mappings_digest: value.mappings_digest,
        })
    }
}
impl NativePublicationComparator for Configured {
    fn compare(&self, job: &NativePublicationJob) -> Result<NativePublicationComparison> {
        if job.registration_digest() != self.registration_digest() {
            return Err(Error::Policy);
        }
        let input = projection::publication(job)?;
        let value: Published =
            self.registration
                .call(Operation::Publish, &input, job.deadline(), || {
                    job.cancelled()
                })?;
        let runtime = job.runtime_job();
        if value.input_digest != job.input_digest()
            || value.runtime_binding != runtime.binding_digest()
            || value.candidate_digest != ContextDigest::of(runtime.candidate().payload())
            || value.attestation.validator_id != self.registration.options.validator_id
            || value.attestation.validator_version != self.registration.options.validator_version
            || value.attestation.completed_turn_id
                != runtime.candidate().coverage().completed_turn_id
        {
            return Err(Error::Mapping);
        }
        let witness =
            WitnessBodyV1::decode(&value.witness.decode(262144)?).map_err(|_| Error::Invalid)?;
        Ok(NativePublicationComparison {
            input_digest: value.input_digest,
            registration_digest: job.registration_digest(),
            candidate_digest: value.candidate_digest,
            attestation: OwnerValidationAttestation {
                validator_id: value.attestation.validator_id,
                validator_version: value.attestation.validator_version,
                completed_turn_id: value.attestation.completed_turn_id,
            },
            witness,
        })
    }
}
