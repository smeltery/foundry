use super::*;
/// Policy bytes and verifier are constructed together; caller cannot assert a digest.
pub struct StorePolicy {
    pub(super) profile: WireProfile,
    pub(super) codes: Option<crate::code_invite::CodePolicy>,
    pub(super) verifier: Option<Arc<ProofVerifier>>,
    pub(super) digest: [u8; 32],
}
impl StorePolicy {
    pub fn new(profile: WireProfile, proof: Option<(ProofConfig, &[u8])>) -> Result<Self> {
        profile.validate().map_err(|_| StoreError::Invalid)?;
        let profile_bytes = checkpoint::encode(&profile)?;
        if profile_bytes.len() > MAX_CHECKPOINT_BYTES {
            return Err(StoreError::TooLarge);
        }
        let mut data = profile_bytes;
        let verifier = match proof {
            None => {
                data.push(0);
                None
            }
            Some((config, jwks)) => {
                data.push(1);
                for value in [
                    config.issuer.as_bytes(),
                    config.audience.as_bytes(),
                    config.token_type.as_bytes(),
                    jwks,
                ] {
                    if value.len() > 256 * 1024 {
                        return Err(StoreError::TooLarge);
                    }
                    data.extend_from_slice(&(value.len() as u64).to_be_bytes());
                    data.extend_from_slice(value);
                }
                Some(Arc::new(
                    ProofVerifier::from_jwks(config, jwks).map_err(|_| StoreError::Invalid)?,
                ))
            }
        };
        Ok(Self {
            codes: None,
            profile,
            verifier,
            digest: digest(&data),
        })
    }
    /// Explicit opt-in binds the actual codebook and contexts, never an asserted digest.
    /// Opening an existing v2 store with this policy does not migrate it.
    pub fn with_codes(mut self, codes: crate::code_invite::CodePolicy) -> Result<Self> {
        if self.codes.is_some() || !codes.matches_profile(&self.profile) {
            return Err(StoreError::Binding);
        }
        let mut bytes = b"foundry/store-code-policy/v1\0".to_vec();
        bytes.extend_from_slice(&self.digest);
        bytes.extend_from_slice(&codes.binding());
        self.digest = digest(&bytes);
        self.codes = Some(codes);
        Ok(self)
    }
}

/// Only the store constructs this after authenticated ancestry and record validation.
pub(crate) struct VerifiedLocalSnapshot<'a> {
    pub(super) document: &'a TeamDocument,
    pub(super) policy: &'a StorePolicy,
}
impl VerifiedLocalSnapshot<'_> {
    pub(crate) fn document(&self) -> &TeamDocument {
        self.document
    }
    pub(crate) fn profile(&self) -> &WireProfile {
        &self.policy.profile
    }
    pub(crate) fn verifier(&self) -> Option<Arc<ProofVerifier>> {
        self.policy.verifier.clone()
    }
}
