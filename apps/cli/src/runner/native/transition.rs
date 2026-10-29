use super::*;
use foundry_runtime::coordinated::{
    CaptureCancelled,
    loading::{
        LoadingCause, LoadingError,
        continuation::{AdmittedProcess, ContinuationCancelled},
    },
};
use foundry_service::{
    ServiceError,
    restoration_owner::{RestorationOwner, RestoreReply},
};
use std::sync::mpsc::Receiver;

type CapturePorts = Box<foundry_service::native_publication::CapturePorts>;
type Admission = std::result::Result<(AdmittedProcess, CapturePorts), ContinuationCancelled>;

pub(super) enum Completion {
    Published,
    CaptureCancelled(Box<CaptureCancelled>),
    TransitionCancelled(ContinuationCancelled),
}

pub(super) fn is_cancelled(error: &LoadingError) -> bool {
    matches!(error.cause, LoadingCause::Cancelled) && error.cleanup.is_none()
}

pub(super) fn capture_ports(
    owner: &mut RestorationOwner,
    admitted: AdmittedProcess,
    configuration: PublicationConfiguration,
    permissions: foundry_service::permission::ToolPermissions,
    cancellation: &Receiver<CancelMode>,
    control: &AtomicU8,
) -> Result<Admission, CommandError> {
    let deadline = Instant::now() + PHASE;
    let submitted = owner.open_capture_with_permissions(configuration, permissions);
    if let Err(error) = submitted {
        if stopped(control) && unavailable(&error) {
            relay(control, |mode| owner.cancel(mode));
            return stop(admitted, cancellation);
        }
        admitted
            .close()
            .map_err(|_| failure("admitted cleanup failed"))?;
        return Err(service_error(error));
    }
    loop {
        relay(control, |mode| owner.cancel(mode));
        if stopped(control) {
            return stop(admitted, cancellation);
        }
        match owner.try_recv() {
            Ok(Some(reply)) => match reply.result {
                Ok(RestoreReply::CaptureReady(ports)) => return Ok(Ok((admitted, ports))),
                Err(error) => {
                    admitted
                        .close()
                        .map_err(|_| failure("admitted cleanup failed"))?;
                    return Err(service_error(error));
                }
                _ => {
                    admitted
                        .close()
                        .map_err(|_| failure("admitted cleanup failed"))?;
                    return Err(failure("capture endpoint absent"));
                }
            },
            Ok(None) => {}
            Err(error) => {
                if stopped(control) && unavailable(&error) {
                    return stop(admitted, cancellation);
                }
                admitted
                    .close()
                    .map_err(|_| failure("admitted cleanup failed"))?;
                return Err(service_error(error));
            }
        }
        if Instant::now() >= deadline {
            owner.cancel(CancelMode::Hard);
            admitted
                .close()
                .map_err(|_| failure("admitted cleanup failed"))?;
            return Err(failure("capture endpoint deadline"));
        }
        thread::sleep(Duration::from_millis(2));
    }
}
fn unavailable(error: &ServiceError) -> bool {
    matches!(
        error,
        ServiceError::Coordination("restore owner stopped" | "restore job queue unavailable")
    )
}
fn stop(
    admitted: AdmittedProcess,
    cancellation: &Receiver<CancelMode>,
) -> Result<Admission, CommandError> {
    admitted
        .close_cancelled(cancellation)
        .map(Err)
        .map_err(|_| failure("admitted cancellation cleanup unverified"))
}
