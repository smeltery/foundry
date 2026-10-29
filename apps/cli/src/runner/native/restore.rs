use super::transition::Completion;
use super::*;
use foundry_contracts::{Session, context::ContextDigest};
use foundry_runtime::coordinated::{CaptureExit, FreshCaptureOptions};
use foundry_service::{
    prepared_adapter::PreparedCaptureParts,
    restoration_owner::{ContinueSelection, RestorationOwner, RestoreReply},
};
use foundry_store::{BeginRestoreInput, Store};

pub(super) fn run(
    database: &Path,
    instruction: (&Session, &str),
    configured: Arc<Configured>,
    files: &artifacts::Files,
    parts: PreparedCaptureParts,
    control: &Arc<AtomicU8>,
    snapshots: foundry_service::cancellation_snapshot::SnapshotSettings,
) -> Result<Session, CommandError> {
    let (session, text) = instruction;
    let mut store = Store::open(database).map_err(|error| service_error(error.into()))?;
    let head = store
        .checkpoint_head(&session.id)
        .map_err(|error| service_error(error.into()))?
        .ok_or_else(|| failure("session has no witnessed checkpoint"))?;
    let message = store
        .enqueue_message(&session.id, &uuid::Uuid::new_v4().to_string(), text)
        .map_err(|error| service_error(error.into()))?;
    let mut owner = RestorationOwner::start_with_snapshot(
        database.into(),
        session.id.clone(),
        BeginRestoreInput {
            request_id: uuid::Uuid::new_v4().to_string(),
            checkpoint_id: head.id,
            expected_session_version: session.version,
            configuration_digest: head.configuration_digest,
        },
        options(),
        snapshots,
    )
    .map_err(service_error)?;
    let message_id = message.id.clone();
    let result = execute(
        &mut owner, session, message, configured, files, parts, control,
    );
    let cancellation = owner.cancellation_handle();
    let report = thread::scope(|scope| {
        let worker = scope.spawn(move || owner.shutdown());
        while !worker.is_finished() {
            relay(control, |mode| cancellation.cancel(mode));
            thread::sleep(Duration::from_millis(2));
        }
        worker
            .join()
            .map_err(|_| failure("restore owner shutdown panicked"))
    })?;
    let mut report = report;
    if let Ok(completion) = &result
        && let Some(settlement) = report.cancellation.as_ref()
        && !report.expired
        && !report.watchdog_panicked
        && report.release.is_ok()
        && settlement.session().id == session.id
        && settlement.message_id() == message_id
        && match completion {
            Completion::TransitionCancelled(stop) => settlement.matches_transition(stop),
            Completion::CaptureCancelled(stop) => settlement.matches_capture(stop),
            Completion::Published => false,
        }
    {
        return Ok(report
            .cancellation
            .take()
            .expect("checked settlement")
            .into_session());
    }
    let incomplete = !matches!(result, Ok(Completion::Published));
    check_report(report, incomplete)?;
    store
        .get(&session.id)
        .map_err(|error| service_error(error.into()))
}

fn execute(
    owner: &mut RestorationOwner,
    session: &Session,
    message: foundry_contracts::Message,
    configured: Arc<Configured>,
    files: &artifacts::Files,
    parts: PreparedCaptureParts,
    control: &AtomicU8,
) -> Result<Completion, CommandError> {
    let cancellation = owner.take_cancellation().map_err(service_error)?;
    let held = loading::load(
        owner,
        &configured,
        session,
        files,
        &parts.spec,
        cancellation,
        control,
    )?;
    let mut process = held.process;
    owner
        .open_continuation(ContinueSelection {
            request_id: uuid::Uuid::new_v4().to_string(),
            message_id: message.id,
            sequence: message.sequence,
            text_digest: ContextDigest::of(message.text.as_bytes()),
        })
        .map_err(service_error)?;
    let RestoreReply::Continuation(endpoint) = loading::receive(owner, control, || {
        process
            .poll(&held.cancellation)
            .map_err(|_| failure("loaded process stopped"))
    })?
    else {
        return Err(failure("continuation endpoint absent"));
    };
    let mut pending = process
        .continue_with(held.binding, endpoint, &held.cancellation)
        .map_err(|_| failure("continuation failed"))?;
    loop {
        relay(control, |mode| owner.cancel(mode));
        match pending.poll(&held.cancellation) {
            Ok(true) => break,
            Ok(false) => {}
            Err(error) if transition::is_cancelled(&error) => {
                let stop = pending
                    .close_cancelled(&held.cancellation)
                    .map_err(|_| failure("continuation cancellation cleanup unverified"))?;
                return Ok(Completion::TransitionCancelled(stop));
            }
            Err(_) => return Err(failure("continuation failed")),
        }
        thread::sleep(Duration::from_millis(2));
    }
    let admitted = pending
        .into_admitted()
        .map_err(|_| failure("continuation not admitted"))?;
    let (admitted, ports) = match transition::capture_ports(
        owner,
        admitted,
        publication(&configured)?,
        parts.permissions,
        &held.cancellation,
        control,
    )? {
        Ok(parts) => parts,
        Err(stop) => return Ok(Completion::TransitionCancelled(stop)),
    };
    let result = thread::scope(|scope| {
        let runtime = scope.spawn(move || {
            admitted.run_restored_capture(
                &parts.spec,
                &ports.session,
                &held.cancellation,
                &ports.messages,
                ports.persistence,
                FreshCaptureOptions {
                    profile: ports.profile,
                    owner: ports.owner,
                    policy: Some(parts.policy),
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
            .map_err(|_| failure("restored runtime panicked"))?
            .map_err(|_| failure("restored runtime failed"))
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
        // The registered cleanup observer and restoration release own suspension.
        CaptureExit::Cancelled(stop) => return Ok(Completion::CaptureCancelled(stop)),
    };
    match (expected, loading::receive(owner, control, || Ok(()))?) {
        (0, RestoreReply::Published(_)) | (1, RestoreReply::ActionRequired(_)) => {
            Ok(Completion::Published)
        }
        _ => Err(failure("restored finalization mismatch")),
    }
}
