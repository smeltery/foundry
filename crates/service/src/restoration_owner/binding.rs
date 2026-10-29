use super::validation::hash_fields;
use foundry_contracts::{
    context::{ContextDigest, ContextReceipt},
    context_observation::ObservationBinding,
};

/// Versioned input field order; raw stored digests and lowercase wire hex are distinct.
pub(super) struct InputCommitment<'a> {
    pub binding: &'a ObservationBinding,
    pub registration: ContextDigest,
    pub record: &'a [u8; 32],
    pub witness: &'a [u8; 32],
    pub stored_configuration: &'a [u8; 32],
    pub effective_configuration: ContextDigest,
    pub payload: &'a [u8; 32],
    pub policy: &'a [u8],
    pub associations: &'a [u8],
    pub origin: &'a [u8],
    pub challenge: &'a ContextReceipt,
    pub instance: &'a ContextReceipt,
    pub observation: ContextDigest,
}
impl InputCommitment<'_> {
    pub fn digest(&self) -> [u8; 32] {
        hash_fields(
            b"foundry.restore.validation-input.v1",
            &[
                self.binding.scope.restore_id.as_str().as_bytes(),
                self.binding.scope.load_id.as_str().as_bytes(),
                self.binding.job_id.as_str().as_bytes(),
                self.binding.begin_binding.to_hex().as_bytes(),
                self.binding.profile_digest.to_hex().as_bytes(),
                self.registration.to_hex().as_bytes(),
                self.record,
                self.witness,
                self.stored_configuration,
                self.effective_configuration.to_hex().as_bytes(),
                self.payload,
                self.policy,
                self.associations,
                self.origin,
                self.challenge.as_str().as_bytes(),
                self.instance.as_str().as_bytes(),
                self.observation.to_hex().as_bytes(),
            ],
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use foundry_contracts::{context::ContextId, context_v2::LoadingScope};
    #[test]
    fn independent_python_golden_pins_field_order_and_effective_configuration() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/native-validation-binding-v1.json"
        ))
        .unwrap();
        let text = |name: &str| fixture[name].as_str().unwrap();
        let receipt = |name| ContextReceipt::new(text(name)).unwrap();
        let digest = |name| ContextDigest::parse(text(name)).unwrap();
        let binding = ObservationBinding {
            scope: LoadingScope {
                restore_id: ContextId::new(text("restore")).unwrap(),
                load_id: receipt("load"),
            },
            job_id: receipt("job"),
            begin_binding: digest("begin"),
            profile_digest: digest("profile"),
        };
        let challenge = receipt("challenge");
        let instance = receipt("instance");
        let mut input = InputCommitment {
            binding: &binding,
            registration: digest("registration"),
            record: &[1; 32],
            witness: &[2; 32],
            stored_configuration: &[3; 32],
            effective_configuration: digest("effective"),
            payload: &[4; 32],
            policy: text("policy").as_bytes(),
            associations: text("associations").as_bytes(),
            origin: text("origin").as_bytes(),
            challenge: &challenge,
            instance: &instance,
            observation: digest("observation"),
        };
        assert!(super::super::validation::digest(input.digest()).to_hex() == text("input_digest"));
        let result = hash_fields(
            b"foundry.restore.validation-result.v1",
            &[
                text("input_digest").as_bytes(),
                text("graph").as_bytes(),
                text("mappings").as_bytes(),
            ],
        );
        assert!(super::super::validation::digest(result).to_hex() == text("result_digest"));
        input.effective_configuration = digest("begin");
        assert!(super::super::validation::digest(input.digest()).to_hex() != text("input_digest"));
    }
}
