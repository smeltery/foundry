use super::{Blob, Error, Registration, Result, StoredProfile, client::Operation, facts};
use foundry_contracts::{
    Session,
    adapter_selection::AdapterSelection,
    context::{ContextDigest, ContextFormat},
    context_observation::ObservationBinding,
    context_v2::{LoadingScope, RestoreDescriptor},
};
use foundry_runtime::coordinated::capture::{ProfileBinding, StoredFormat};
use foundry_store::RestoreBaselineSnapshot;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Instant};

#[derive(Clone, Serialize)]
pub struct Artifacts {
    pub plan_path: PathBuf,
    pub producer_path: PathBuf,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    stored: StoredProfile,
    wire: ContextFormat,
    configuration_digest: ContextDigest,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    selection_digest: ContextDigest,
    profile: Profile,
    policy: Blob,
    associations: Blob,
    observation_profile_digest: ContextDigest,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    plan: Blob,
}

pub struct Configured {
    pub(super) registration: Arc<Registration>,
    pub(super) selection: AdapterSelection,
    artifacts: Artifacts,
    configuration: Configuration,
}
pub struct RestorePreparation<'a> {
    pub scope: &'a LoadingScope,
    pub descriptor: &'a RestoreDescriptor,
    pub observation: &'a ObservationBinding,
    pub baseline: &'a RestoreBaselineSnapshot,
}
impl Registration {
    pub fn configure(
        self: Arc<Self>,
        selection: AdapterSelection,
        artifacts: Artifacts,
        deadline: Instant,
        cancelled: impl Fn() -> bool,
    ) -> Result<Arc<Configured>> {
        let digest = selection.digest().map_err(|_| Error::Policy)?;
        for path in [&artifacts.plan_path, &artifacts.producer_path] {
            if !path.is_absolute() || path.to_str().is_none_or(|s| s.len() > 4096) {
                return Err(Error::Policy);
            }
        }
        if artifacts.plan_path == artifacts.producer_path
            || artifacts.plan_path.parent() != artifacts.producer_path.parent()
        {
            return Err(Error::Policy);
        }
        let input = json!({"selection":selection,"configuration":Blob::new(&self.options.configuration,65536)?,"artifacts":artifacts});
        let configured: Configuration =
            self.call(Operation::Configure, &input, deadline, cancelled)?;
        if configured.selection_digest != digest
            || configured.profile.configuration_digest != digest
            || configured.observation_profile_digest != self.options.observation_profile_digest
            || configured.profile.stored != self.options.stored
            || configured.profile.wire.profile != self.options.wire.profile
            || configured.profile.wire.format_version != self.options.wire.format_version
            || configured.profile.wire.producer_build != self.options.wire.producer_build
        {
            return Err(Error::Policy);
        }
        configured.policy.decode(65536)?;
        configured.associations.decode(262144)?;
        Ok(Arc::new(Configured {
            registration: self,
            selection,
            artifacts,
            configuration: configured,
        }))
    }
}
impl Configured {
    pub fn profile(&self) -> ProfileBinding {
        let f = &self.configuration.profile.stored;
        ProfileBinding {
            stored: StoredFormat {
                format_id: f.format_id.clone(),
                format_version: f.format_version,
                profile_id: f.profile_id.clone(),
                binding_id: f.binding_id.clone(),
                binding_version: f.binding_version.clone(),
            },
            wire: self.configuration.profile.wire.clone(),
            configuration_digest: *self.configuration.selection_digest.as_bytes(),
        }
    }
    pub fn registration_digest(&self) -> ContextDigest {
        self.registration.options.registration_digest
    }
    pub fn observation_profile_digest(&self) -> ContextDigest {
        self.configuration.observation_profile_digest
    }
    pub fn policy(&self) -> Result<Vec<u8>> {
        self.configuration.policy.decode(65536)
    }
    pub fn associations(&self) -> Result<Vec<u8>> {
        self.configuration.associations.decode(262144)
    }
    fn projection(&self) -> Result<Value> {
        Ok(
            json!({"selection":self.selection,"configuration":Blob::new(&self.registration.options.configuration,65536)?,"artifacts":self.artifacts,"profile":self.configuration.profile,"policy":self.configuration.policy,"associations":self.configuration.associations,"observation_profile_digest":self.configuration.observation_profile_digest}),
        )
    }
    pub fn prepare_fresh(
        &self,
        session: &Session,
        attempt: &str,
        deadline: Instant,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<u8>> {
        let launch = json!({"kind":"fresh","attempt_id":attempt,"format_digest":self.profile().wire_digest().map_err(|_|Error::Policy)?});
        self.prepare(session, launch, deadline, cancelled)
    }
    pub fn prepare_restore(
        &self,
        session: &Session,
        restore: RestorePreparation<'_>,
        deadline: Instant,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<u8>> {
        let launch = json!({"kind":"restore","scope":restore.scope,"descriptor":restore.descriptor,"observation":restore.observation,"baseline":facts::baseline(restore.baseline)?});
        self.prepare(session, launch, deadline, cancelled)
    }
    fn prepare(
        &self,
        session: &Session,
        launch: Value,
        deadline: Instant,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<u8>> {
        let input = json!({"configured":self.projection()?,"session":session,"launch":launch});
        let plan: Plan = self
            .registration
            .call(Operation::Prepare, &input, deadline, cancelled)?;
        plan.plan.decode(1048576)
    }
}
