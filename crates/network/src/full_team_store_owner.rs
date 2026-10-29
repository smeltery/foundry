use super::*;
use crate::daemon_lease::DaemonLease;

impl FullTeamStore {
    pub(crate) fn wire_profile(&self) -> &WireProfile {
        &self.policy.profile
    }
    /// Check assembly against the protected store's actual root and local key.
    /// No root path or secret material is exported to the caller.
    pub(crate) fn verify_owner(
        &self,
        ownership: &DaemonLease,
        local: iroh::EndpointId,
    ) -> Result<()> {
        #[cfg(unix)]
        {
            if self.identity.endpoint_id() != local {
                return Err(StoreError::Binding);
            }
            self.protection.check()?;
            let path = self.protection.path().parent().ok_or(StoreError::Binding)?;
            let root = cap_std::fs::Dir::open_ambient_dir(path, cap_std::ambient_authority())
                .map_err(|_| StoreError::UnsafeStorage)?;
            ownership.check_root(&root).map_err(|_| StoreError::Binding)
        }
        #[cfg(not(unix))]
        {
            let _ = (ownership, local);
            Err(StoreError::Unsupported)
        }
    }
}
