//! Trusted library integration for initial fresh attempts only.
use super::{capture::ProfileBinding, capture_owner::CaptureOwner, *};

pub struct FreshCaptureOptions {
    pub profile: ProfileBinding,
    pub owner: CaptureOwner,
    pub policy: Option<TrustedToolPolicy>,
    pub phase_timeout: Duration,
}

/// Prepared is private handoff authority, never durable task completion.
pub enum CaptureExit {
    Prepared(Box<PreparedCapture>),
    Cancelled { mode: CancelMode, forced: bool },
}

/// The linked owner must require a registered native semantic validator, establish
/// an initial fresh attempt, and keep its lease alive through durable publication.
/// This function sends Start, never Restore/replay, and cannot establish a restored
/// baseline. Restored capture remains required future work, not supported fallback.
/// No CLI selection or implicit validator is installed by this library interface.
pub fn run_fresh_capture(
    spec: &AdapterSpec,
    session: &Session,
    attempt_id: &str,
    cancellation: &Receiver<CancelMode>,
    messages: &Receiver<AdapterMessage>,
    persistence: RuntimePersistence,
    options: FreshCaptureOptions,
) -> Result<CaptureExit, RuntimeError> {
    match engine::run_owned(
        spec,
        session,
        attempt_id,
        cancellation,
        messages,
        persistence,
        ingress::ExecutionOptions {
            mode: ingress::ExecutionMode::CaptureStartup(
                startup::StartupPlan {
                    profile: Box::new(options.profile),
                    timeout: options.phase_timeout,
                },
                options.owner,
            ),
            policy: options.policy,
        },
    )? {
        engine::EngineExit::Prepared(prepared) => Ok(CaptureExit::Prepared(prepared)),
        engine::EngineExit::Ordinary(RunResult::Cancelled { mode, forced }) => {
            Ok(CaptureExit::Cancelled { mode, forced })
        }
        engine::EngineExit::Ordinary(RunResult::Completed { .. }) => Err(RuntimeError::Protocol(
            "ordinary completion is forbidden during capture",
        )),
    }
}
