use super::*;
use foundry_contracts::{
    Session, context::ContextPayload, context_observation::OBSERVE_CAPABILITY,
};
use foundry_runtime::{
    AdapterSpec,
    coordinated::loading::{continuation::ContinuationBinding, *},
};
use foundry_service::{
    native_validation::*,
    profile_host::RestorePreparation,
    restoration_owner::{RestorationOwner, RestoreReply},
};
use std::sync::mpsc::Receiver;

pub(super) struct Loaded {
    pub process: LoadedProcess,
    pub binding: ContinuationBinding,
    pub cancellation: Receiver<CancelMode>,
}
pub(super) fn receive(
    owner: &mut RestorationOwner,
    control: &AtomicU8,
    mut poll: impl FnMut() -> Result<(), CommandError>,
) -> Result<RestoreReply, CommandError> {
    let deadline = Instant::now() + PHASE;
    loop {
        relay(control, |mode| owner.cancel(mode));
        poll()?;
        if let Some(reply) = owner.try_recv().map_err(service_error)? {
            return reply.result.map_err(service_error);
        }
        if Instant::now() >= deadline {
            owner.cancel(CancelMode::Hard);
            return Err(failure("restore owner deadline"));
        }
        thread::sleep(Duration::from_millis(2));
    }
}
pub(super) fn load(
    owner: &mut RestorationOwner,
    configured: &Arc<Configured>,
    session: &Session,
    files: &artifacts::Files,
    spec: &AdapterSpec,
    cancellation: Receiver<CancelMode>,
    control: &AtomicU8,
) -> Result<Loaded, CommandError> {
    let RestoreReply::Reserved { scope, .. } = receive(owner, control, || Ok(()))? else {
        return Err(failure("restore reservation absent"));
    };
    owner.request_baseline().map_err(service_error)?;
    let RestoreReply::Baseline(snapshot) = receive(owner, control, || Ok(()))? else {
        return Err(failure("restore baseline absent"));
    };
    owner
        .bind_plan(origin::descriptor(&snapshot)?)
        .map_err(service_error)?;
    let RestoreReply::PlanBound(descriptor) = receive(owner, control, || Ok(()))? else {
        return Err(failure("restore plan unbound"));
    };
    let profile = configured.observation_profile_digest();
    let baseline = origin::origin(&snapshot, profile)?;
    let effective = configured
        .profile()
        .configuration_wire_digest()
        .map_err(|_| failure("invalid profile digest"))?;
    owner
        .prepare_native(
            NativeValidatorRegistration {
                identity: configured.registration_digest(),
                timeout: PHASE,
                comparator: configured.clone(),
            },
            NativeExpectations {
                profile_digest: profile,
                configuration_digest: effective,
                policy: configured
                    .policy()
                    .map_err(|_| failure("invalid profile policy"))?,
                associations: configured
                    .associations()
                    .map_err(|_| failure("invalid profile associations"))?,
                origin: OriginIdentity::decode(
                    &baseline.encode().map_err(|_| failure("invalid origin"))?,
                )
                .map_err(|_| failure("invalid origin"))?,
            },
        )
        .map_err(service_error)?;
    let RestoreReply::NativePrepared(observation) = receive(owner, control, || Ok(()))? else {
        return Err(failure("observation job absent"));
    };
    let bytes = configured
        .prepare_restore(
            session,
            RestorePreparation {
                scope: &scope,
                descriptor: &descriptor,
                observation: &observation,
                baseline: &snapshot,
            },
            Instant::now() + PHASE,
            || stopped(control),
        )
        .map_err(|_| failure("restore preparation failed"))?;
    files.write_plan(&bytes)?;
    let payload = ContextPayload::new(snapshot.checkpoint().payload().to_vec())
        .map_err(|_| failure("invalid restore payload"))?;
    let plan = LoadingPlan::new(
        session.directory.clone().into(),
        scope,
        *descriptor,
        payload,
        LoadingLimits {
            phase_timeout: PHASE,
            retained_timeout: Duration::from_secs(120),
        },
        LoadingPurpose::PrepareContinuation,
        vec![OBSERVE_CAPABILITY.into()],
    )
    .map_err(|_| failure("invalid loading plan"))?;
    // The caller retains the owner while this thread owns the sole cancellation receiver.
    let (loaded, cancellation) = thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let result = foundry_runtime::coordinated::loading::load(spec, plan, &cancellation);
            (result, cancellation)
        });
        while !worker.is_finished() {
            relay(control, |mode| owner.cancel(mode));
            thread::sleep(Duration::from_millis(2));
        }
        worker
            .join()
            .map_err(|_| failure("loading runtime panicked"))
    })?;
    let mut process = loaded.map_err(|_| failure("loading runtime failed"))?;
    owner.register_loaded(&process).map_err(service_error)?;
    if !matches!(
        receive(owner, control, || process
            .poll(&cancellation)
            .map_err(|_| failure("loaded process stopped")))?,
        RestoreReply::CleanupRegistered
    ) {
        return Err(failure("cleanup registration absent"));
    }
    process
        .begin_observation(observation.clone(), &cancellation)
        .map_err(|_| failure("observation start failed"))?;
    let frame = loop {
        relay(control, |mode| owner.cancel(mode));
        if let Some(frame) = process
            .poll_observation(&cancellation)
            .map_err(|_| failure("observation failed"))?
        {
            break frame;
        }
        thread::sleep(Duration::from_millis(2));
    };
    let instance_id = frame
        .instance_id()
        .ok_or_else(|| failure("observation instance absent"))?
        .clone();
    owner
        .validate_native(&process, frame)
        .map_err(service_error)?;
    if !matches!(
        receive(owner, control, || process
            .poll(&cancellation)
            .map_err(|_| failure("loaded process stopped")))?,
        RestoreReply::NativeStarted
    ) {
        return Err(failure("validation start absent"));
    }
    loop {
        owner.native_status().map_err(service_error)?;
        match receive(owner, control, || {
            process
                .poll(&cancellation)
                .map_err(|_| failure("loaded process stopped"))
        })? {
            RestoreReply::NativeValidated(_) => break,
            RestoreReply::NativePending => thread::sleep(Duration::from_millis(2)),
            _ => return Err(failure("validation phase mismatch")),
        }
    }
    Ok(Loaded {
        process,
        cancellation,
        binding: ContinuationBinding {
            observation,
            instance_id,
            baseline,
            effective_configuration_digest: effective,
        },
    })
}
use foundry_runtime::coordinated::origin::OriginIdentity;
