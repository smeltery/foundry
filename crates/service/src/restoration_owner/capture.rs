//! Consuming capture handoffs share the restoration owner's job queue.
use super::*;

impl RestorationOwner {
    pub fn open_capture(
        &mut self,
        configuration: crate::native_publication::PublicationConfiguration,
    ) -> Result<ContextReceipt> {
        self.submit(JobKind::OpenCapture(Box::new(configuration), None))
    }
    /// The runtime must use the matching policy in required authorization mode.
    pub fn open_capture_with_permissions(
        &mut self,
        configuration: crate::native_publication::PublicationConfiguration,
        permissions: crate::permission::ToolPermissions,
    ) -> Result<ContextReceipt> {
        permissions.check_configuration(&configuration.profile.configuration_digest)?;
        self.submit(JobKind::OpenCapture(
            Box::new(configuration),
            Some(Arc::new(permissions)),
        ))
    }
    pub fn finish_capture(
        &mut self,
        prepared: foundry_runtime::coordinated::PreparedCapture,
    ) -> Result<ContextReceipt> {
        self.submit(JobKind::FinishCapture(Box::new(prepared)))
    }

    pub fn finish_action_required(
        &mut self,
        stop: foundry_runtime::coordinated::CaptureActionRequired,
    ) -> Result<ContextReceipt> {
        self.submit(JobKind::FinishActionRequired(Box::new(stop)))
    }
}
