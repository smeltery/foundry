//! Completion criteria under `foundry.completion.v1` (completion-policy.md).
//!
//! Admission fixes the attempt's criteria and configuration digest. After the
//! adapter settles with a `success` proposal, the runtime runs each admitted
//! check under the attempt fence and records the results; the store then
//! decides the outcome in the terminal transaction. Adapter text, events and
//! tool output are never read as evidence.
use crate::{Result, ServiceError, completion_check};
use foundry_contracts::{
    CompletionCheck, CompletionCriteria, ContractError, Session,
    session_v2::COMPLETION_POLICY_VERSION,
};
use foundry_runtime::AdapterSpec;
use foundry_store::{AttemptFence, CompletionAdmission, CompletionEvidence, Store, StoreError};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// How often a running check rechecks durable cancellation.
const CANCELLATION_POLL: Duration = Duration::from_millis(100);

/// Profile defaults first; an inline check with the same `id` replaces the
/// default in place and any other inline check is appended.
pub fn admitted_checks(
    defaults: Vec<CompletionCheck>,
    inline: &[CompletionCheck],
) -> Vec<CompletionCheck> {
    let mut checks = defaults;
    for check in inline {
        match checks.iter_mut().find(|current| current.id() == check.id()) {
            Some(current) => *current = check.clone(),
            None => checks.push(check.clone()),
        }
    }
    checks
}

/// No configuration profile source exists yet, so a named profile cannot
/// supply defaults. Session creation already refuses profile references.
fn profile_defaults(profile: Option<&str>) -> Result<Vec<CompletionCheck>> {
    match profile {
        None => Ok(Vec::new()),
        Some(_) => Err(ServiceError::Store(StoreError::Contract(
            ContractError::Unsupported("completion criteria profile".into()),
        ))),
    }
}

/// Digest of the adapter, model and permission configuration admitted for the attempt.
pub(crate) fn configuration_digest(
    spec: &AdapterSpec,
    session: &Session,
    tools: bool,
    permissions: bool,
) -> Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut field = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    };
    field(b"foundry.completion.configuration.v1");
    field(spec.executable.as_os_str().as_encoded_bytes());
    for argument in &spec.args {
        field(argument.as_encoded_bytes());
    }
    for timeout in [spec.ready_timeout, spec.run_timeout, spec.cancel_timeout] {
        field(&timeout.as_millis().to_be_bytes());
    }
    field(&serde_json::to_vec(&session.model).map_err(StoreError::from)?);
    field(&[u8::from(tools), u8::from(permissions)]);
    Ok(hash.finalize().into())
}

/// Fix the criteria for a newly admitted attempt. A continuation inherits the
/// session's criteria unless `declared` supplies its own set.
pub(crate) fn admit(
    store: &mut Store,
    fence: &AttemptFence,
    configuration_digest: [u8; 32],
    declared: Option<CompletionCriteria>,
) -> Result<()> {
    let criteria = match declared {
        Some(criteria) => Some(criteria),
        None => store.session_completion(&fence.session_id)?,
    };
    let checks = match criteria {
        Some(criteria) => admitted_checks(
            profile_defaults(criteria.profile.as_deref())?,
            &criteria.checks,
        ),
        None => Vec::new(),
    };
    let admission = CompletionAdmission::new(&fence.attempt_id, configuration_digest, checks)?;
    Ok(store.admit_completion(fence, admission)?)
}

/// Run the admitted checks in `workspace` and record their results under the
/// live fence. `stopped` reports local cancellation or lease loss; durable
/// cancellation is polled here. A stopped run records nothing as passed.
pub(crate) fn verify(
    store: &mut Store,
    fence: &AttemptFence,
    workspace: &Path,
    stopped: &dyn Fn() -> bool,
) -> Result<()> {
    let Some(admission) = store.completion_admission(fence).cloned() else {
        return Ok(());
    };
    if admission.checks().is_empty() {
        return Ok(());
    }
    let work_revision = store.work_revision(&fence.session_id)?;
    let mut polled = Instant::now();
    let mut cancelled = store.cancellation(fence)?.is_some();
    let mut stop = || {
        if !cancelled && polled.elapsed() >= CANCELLATION_POLL {
            polled = Instant::now();
            // A lost fence also stops the check; finalization reports the error.
            cancelled = !matches!(store.cancellation(fence), Ok(None));
        }
        cancelled || stopped()
    };
    let mut results = Vec::with_capacity(admission.checks().len());
    for check in admission.checks() {
        if stop() {
            break;
        }
        results.push(completion_check::run(workspace, check, &mut stop));
    }
    let evidence = CompletionEvidence {
        policy_version: COMPLETION_POLICY_VERSION.into(),
        attempt_id: fence.attempt_id.clone(),
        configuration_digest: admission.configuration_digest(),
        criteria_digest: admission.criteria_digest(),
        work_revision,
        results,
    };
    Ok(store.record_completion_evidence(fence, evidence)?)
}
