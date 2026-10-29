use super::*;
use foundry_contracts::Session;
use foundry_runtime::coordinated::{CaptureExit, FreshCaptureOptions, run_fresh_capture};
use foundry_service::{
    fresh_capture_owner::{FreshCaptureOwner, FreshCaptureReply},
    prepared_adapter::PreparedCaptureParts,
};

pub(super) fn run(
    database: &Path,
    session: &str,
    configured: Arc<Configured>,
    files: &artifacts::Files,
    parts: PreparedCaptureParts,
    control: &Arc<AtomicU8>,
    snapshots: foundry_service::cancellation_snapshot::SnapshotSettings,
) -> Result<Session, CommandError> {
    let finalization_timeout = Duration::from_millis(snapshots.limits.timeout_ms)
        .checked_add(options().cleanup_timeout)
        .ok_or_else(|| failure("snapshot deadline overflow"))?
        .max(PHASE);
    let mut owner = FreshCaptureOwner::start_with_settings(
        database.into(),
        session.into(),
        publication(&configured)?,
        options(),
        Some(parts.permissions),
        Some(snapshots),
    )
    .map_err(service_error)?;
    let result = execute(
        &mut owner,
        configured,
        files,
        parts.spec,
        parts.policy,
        control,
        finalization_timeout,
    );
    let report = owner.shutdown();
    check_report(report, result.is_err())?;
    result?;
    Service::open(database)
        .and_then(|service| service.get(session))
        .map_err(service_error)
}
fn receive(
    owner: &mut FreshCaptureOwner,
    control: &AtomicU8,
    timeout: Duration,
) -> Result<FreshCaptureReply, CommandError> {
    let deadline = Instant::now() + timeout;
    loop {
        relay(control, |mode| owner.cancel(mode));
        if let Some(reply) = owner.try_recv().map_err(service_error)? {
            return Ok(reply);
        }
        if Instant::now() >= deadline {
            owner.cancel(CancelMode::Hard);
            return Err(failure("capture owner deadline"));
        }
        thread::sleep(Duration::from_millis(2));
    }
}
fn execute(
    owner: &mut FreshCaptureOwner,
    configured: Arc<Configured>,
    files: &artifacts::Files,
    spec: foundry_runtime::AdapterSpec,
    policy: foundry_runtime::coordinated::TrustedToolPolicy,
    control: &AtomicU8,
    finalization_timeout: Duration,
) -> Result<(), CommandError> {
    let cancellation = owner.take_cancellation().map_err(service_error)?;
    let FreshCaptureReply::Ready(ports) = receive(owner, control, PHASE)? else {
        return Err(failure("capture owner phase mismatch"));
    };
    let plan = configured
        .prepare_fresh(
            &ports.session,
            &ports.attempt_id,
            Instant::now() + PHASE,
            || stopped(control),
        )
        .map_err(|_| failure("capture preparation failed"))?;
    files.write_plan(&plan)?;
    let result = thread::scope(|scope| {
        let runtime = scope.spawn(move || {
            run_fresh_capture(
                &spec,
                &ports.session,
                &ports.attempt_id,
                &cancellation,
                &ports.messages,
                ports.persistence,
                FreshCaptureOptions {
                    profile: ports.profile,
                    owner: ports.owner,
                    policy: Some(policy),
                    phase_timeout: PHASE,
                },
            )
        });
        while !runtime.is_finished() {
            relay(control, |mode| owner.cancel(mode));
            thread::sleep(Duration::from_millis(2));
        }
        runtime
            .join()
            .map_err(|_| failure("capture runtime panicked"))?
            .map_err(|_| failure("capture runtime failed"))
    })?;
    let expected = match result {
        CaptureExit::Prepared(prepared) => {
            owner.finish_capture(*prepared).map_err(service_error)?;
            0
        }
        CaptureExit::ActionRequired(stop) => {
            owner.finish_action_required(*stop).map_err(service_error)?;
            1
        }
        CaptureExit::Cancelled(stop) => {
            owner.finish_cancelled(*stop).map_err(service_error)?;
            2
        }
    };
    match (expected, receive(owner, control, finalization_timeout)?) {
        (0, FreshCaptureReply::Published(_))
        | (1, FreshCaptureReply::ActionRequired(_))
        | (2, FreshCaptureReply::Cancelled(_)) => Ok(()),
        _ => Err(failure("capture finalization phase mismatch")),
    }
}
