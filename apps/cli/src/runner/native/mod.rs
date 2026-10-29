use super::{native_registration, prepared, service_error, terminal_response};
use crate::{CommandError, Response};
use foundry_contracts::CancelMode;
use foundry_service::{
    Service, ServiceOptions,
    native_publication::{NativePublicationRegistration, PublicationConfiguration},
    profile_host::Configured,
    restoration_owner::RestoreOwnerOptions,
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
mod artifacts;
mod fresh;
mod loading;
mod origin;
mod report;
mod restore;
use report::check_report;
const PHASE: Duration = Duration::from_secs(30);

pub(super) fn run(
    database: &Path,
    registration: &Path,
    resolved: prepared::Resolved,
    control: &Arc<AtomicU8>,
    resumed: Option<foundry_contracts::Session>,
) -> Result<Response, CommandError> {
    let registration = Arc::new(native_registration::load(
        registration,
        &resolved.private.system_prompt,
    )?);
    let files = artifacts::Files::create(&resolved.private.agent_directory)?;
    let configured = registration
        .configure(
            resolved.selection.clone(),
            files.paths.clone(),
            Instant::now() + PHASE,
            || stopped(control),
        )
        .map_err(|_| failure("profile configuration failed"))?;
    // Resolve all prepared policy/bootstrap inputs before creating a session.
    let request = resolved.request.clone();
    let snapshots = foundry_service::cancellation_snapshot::SnapshotSettings::for_database(
        resolved.helper().to_path_buf(),
        database,
    )
    .map_err(service_error)?;
    let parts = resolved
        .prepare(Some(&files.paths.plan_path))?
        .into_capture_parts()
        .map_err(service_error)?;
    if stopped(control) {
        return Err(failure("capture cancelled before admission"));
    }
    let mut service = Service::open(database).map_err(service_error)?;
    let (session, result) = match resumed {
        Some(session) => {
            let result = restore::run(
                database,
                (&session, &request.prompt),
                configured,
                &files,
                parts,
                control,
                snapshots,
            );
            (session, result)
        }
        None => {
            let session = service.create(request).map_err(service_error)?;
            let result = fresh::run(
                database,
                &session.id,
                configured,
                &files,
                parts,
                control,
                snapshots,
            );
            (session, result)
        }
    };
    match result {
        Ok(session) => {
            files.remove()?;
            terminal_response(session)
        }
        // Post-admission failure cannot invent cleanup authority. Retain artifacts.
        Err(mut error) => {
            error.message = format!("{} (session {})", error.message, session.id);
            Err(error)
        }
    }
}
fn publication(configured: &Arc<Configured>) -> Result<PublicationConfiguration, CommandError> {
    Ok(PublicationConfiguration {
        profile: configured.profile(),
        policy: configured
            .policy()
            .map_err(|_| failure("profile policy invalid"))?,
        associations: configured
            .associations()
            .map_err(|_| failure("profile associations invalid"))?,
        validator: NativePublicationRegistration {
            identity: configured.registration_digest(),
            timeout: PHASE,
            comparator: configured.clone(),
        },
    })
}
fn options() -> RestoreOwnerOptions {
    RestoreOwnerOptions {
        lease: ServiceOptions {
            lease_duration: Duration::from_secs(30),
            poll_interval: Duration::from_millis(50),
        },
        cleanup_timeout: Duration::from_secs(5),
    }
}
fn stopped(control: &AtomicU8) -> bool {
    control.load(Ordering::Acquire) != 0
}
fn relay(control: &AtomicU8, cancel: impl FnOnce(CancelMode)) {
    match control.load(Ordering::Acquire) {
        0 => {}
        1 => cancel(CancelMode::Graceful),
        _ => cancel(CancelMode::Hard),
    }
}
fn failure(message: &str) -> CommandError {
    CommandError::new("execution", message)
}

pub(super) fn resume<B: crate::Backend>(
    backend: &super::ExecutionBackend<B>,
    mut request: crate::Request,
    id: &str,
) -> Result<Response, CommandError> {
    let registration = backend
        .native_profile
        .as_deref()
        .ok_or_else(|| super::unsupported("resume without a registered native profile"))?;
    let configuration = backend
        .adapter_configuration
        .as_deref()
        .ok_or_else(|| failure("resume requires private adapter configuration"))?;
    let executable = backend
        .adapter
        .as_deref()
        .ok_or_else(|| failure("resume requires an adapter executable"))?;
    super::existing_database(&backend.database)?;
    let session = Service::open(&backend.database)
        .and_then(|service| service.get(id))
        .map_err(service_error)?;
    let values = &mut request.command["arguments"]["options"];
    values["resume"] = serde_json::Value::Null;
    if values["dir"].is_null() {
        values["dir"] = session.directory.clone().into();
    }
    let task = super::task::resolve(&request, true)?;
    let resolved = prepared::resolve(
        task,
        executable,
        configuration,
        backend.toolhost_override.as_deref(),
    )?;
    if resolved.request.directory != session.directory || resolved.request.model != session.model {
        return Err(super::invalid(
            "resume selection differs from original session",
        ));
    }
    run(
        &backend.database,
        registration,
        resolved,
        &backend.control,
        Some(session),
    )
}
