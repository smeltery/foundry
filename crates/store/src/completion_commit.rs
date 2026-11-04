//! The decision step of foundry.completion.v1, run inside the terminal transaction.
use crate::completion_decisions::{
    CompletionAdmission, CompletionEvidence, CriterionStatus, DecisionBasis, DecisionInput,
    Proposal, criteria_digest, session_criteria,
};
use crate::{AttemptFence, Result, checkpoint_cut, conflict, now_ms};
use foundry_contracts::{Outcome, session_v2::COMPLETION_POLICY_VERSION};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

/// Fallback when no admission was staged on this connection: the session's
/// declared inline checks, bound to its stored model selection. No evidence can
/// match it unless the runtime admitted it, so `success` stays unreachable.
fn default_admission(connection: &Connection, fence: &AttemptFence) -> Result<CompletionAdmission> {
    let checks = session_criteria(connection, &fence.session_id)?
        .map(|criteria| criteria.checks)
        .unwrap_or_default();
    let session = crate::read_session(connection, &fence.session_id)?;
    let model = serde_json::to_vec(&session.model)?;
    let configuration = checkpoint_cut::digest(b"foundry.completion.unadmitted.v1", &[&model]);
    CompletionAdmission::new(fence.attempt_id.clone(), configuration, checks)
}

struct Decision {
    outcome: Outcome,
    basis: DecisionBasis,
    cause: &'static str,
    results: Value,
}

fn evaluate(
    input: &DecisionInput<'_>,
    admission: &CompletionAdmission,
    newer_pending: bool,
    work_revision: u64,
) -> Decision {
    use DecisionBasis::{AdapterDowngrade, Default as DefaultBasis, Verified};
    use Outcome::{ActionRequired, Failure, Success};
    let lowered = match input.proposal {
        _ if newer_pending => Some((ActionRequired, DefaultBasis, "newer_instruction_pending")),
        Proposal::PermissionStop => Some((
            ActionRequired,
            AdapterDowngrade,
            "permission_action_required",
        )),
        Proposal::Adapter(Failure) => Some((Failure, AdapterDowngrade, "adapter_failure")),
        Proposal::Adapter(ActionRequired) => {
            Some((ActionRequired, AdapterDowngrade, "adapter_action_required"))
        }
        Proposal::Adapter(Success) => None,
    };
    if let Some((outcome, basis, cause)) = lowered {
        return Decision {
            outcome,
            basis,
            cause,
            results: json!([]),
        };
    }
    let current = input
        .evidence
        .filter(|evidence| current_evidence(evidence, admission, work_revision));
    let absent = if input.evidence.is_some() && current.is_none() {
        "stale"
    } else {
        "missing"
    };
    let mut failed = false;
    let mut verified = !admission.checks().is_empty();
    let results = admission
        .checks()
        .iter()
        .map(|check| {
            let (status, detail) = match current
                .and_then(|evidence| evidence.results.iter().find(|r| r.id == check.id()))
            {
                Some(result) => (result.status, result.detail),
                None => (CriterionStatus::Unverified, absent),
            };
            failed |= status == CriterionStatus::Failed;
            verified &= status == CriterionStatus::Passed;
            json!({"id": check.id(), "status": status_name(status), "detail": detail})
        })
        .collect();
    let (outcome, basis, cause) = match (failed, verified) {
        (true, _) => (Failure, Verified, "criterion_failed"),
        (false, true) => (Success, Verified, "criteria_verified"),
        (false, false) => (ActionRequired, DefaultBasis, "criteria_unverified"),
    };
    Decision {
        outcome,
        basis,
        cause,
        results,
    }
}

/// Evidence counts only when written for this attempt against the exact
/// admitted binding and the work revision current at finalization.
fn current_evidence(
    evidence: &CompletionEvidence,
    admission: &CompletionAdmission,
    work_revision: u64,
) -> bool {
    let mut ids = std::collections::HashSet::new();
    evidence.policy_version == COMPLETION_POLICY_VERSION
        && evidence.attempt_id == admission.attempt_id()
        && evidence.configuration_digest == admission.configuration_digest()
        && evidence.criteria_digest == admission.criteria_digest()
        && evidence.work_revision == work_revision
        && evidence.results.iter().all(|result| ids.insert(&result.id))
}

fn status_name(status: CriterionStatus) -> &'static str {
    match status {
        CriterionStatus::Passed => "passed",
        CriterionStatus::Failed => "failed",
        CriterionStatus::Unverified => "unverified",
    }
}

/// Decide inside the terminal transaction after the fence, cancellation and
/// unresolved-tool checks, and persist the row. Returns the stored outcome.
pub(crate) fn commit(
    tx: &rusqlite::Transaction<'_>,
    fence: &AttemptFence,
    input: &DecisionInput<'_>,
) -> Result<Outcome> {
    let fallback;
    let admission = match input.admission {
        Some(admission) if admission.attempt_id() == fence.attempt_id => admission,
        Some(_) => return Err(conflict("completion admission belongs to another attempt")),
        None => {
            fallback = default_admission(tx, fence)?;
            &fallback
        }
    };
    if criteria_digest(admission.checks())? != admission.criteria_digest() {
        return Err(conflict("completion criteria digest mismatch"));
    }
    let newer_pending: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE session_id=?1 AND state IN ('pending','in_flight'))",
        [&fence.session_id],
        |row| row.get(0),
    )?;
    let work_revision: i64 = tx.query_row(
        "SELECT revision FROM session_work WHERE session_id=?1",
        [&fence.session_id],
        |row| row.get(0),
    )?;
    let event_sequence: i64 = tx.query_row(
        "SELECT COALESCE(MAX(sequence),0) FROM events WHERE session_id=?1",
        [&fence.session_id],
        |row| row.get(0),
    )?;
    let revision =
        u64::try_from(work_revision).map_err(|_| conflict("session work revision is invalid"))?;
    let decision = evaluate(input, admission, newer_pending, revision);
    tx.execute(
        "INSERT INTO completion_decisions(attempt_id,session_id,policy_version,configuration_digest,
         criteria_digest,work_revision,event_sequence,checkpoint_id,outcome,basis,cause,
         criterion_results,committed_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            fence.attempt_id,
            fence.session_id,
            COMPLETION_POLICY_VERSION,
            admission.configuration_digest().as_slice(),
            admission.criteria_digest().as_slice(),
            work_revision,
            event_sequence,
            input.checkpoint_id,
            outcome_name(decision.outcome),
            decision.basis.as_str(),
            decision.cause,
            decision.results.to_string(),
            now_ms()?,
        ],
    )?;
    Ok(decision.outcome)
}

fn outcome_name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Success => "success",
        Outcome::Failure => "failure",
        Outcome::ActionRequired => "action_required",
    }
}
