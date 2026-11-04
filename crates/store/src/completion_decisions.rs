//! Completion decisions under `foundry.completion.v1` (completion-policy.md).
//!
//! Every `exit` terminal commits exactly one decision row in the terminal
//! transaction. The adapter's outcome is only a proposal: it may lower the
//! outcome, while `success` needs runtime-verified evidence that matches the
//! criteria and configuration admitted for this attempt and the current work
//! revision. Records written before the policy have no row and are not rewritten.
use crate::{AttemptFence, Result, Store, attempts::check_fence, checkpoint_cut, conflict};
use foundry_contracts::{
    CompletionCheck, ContractError, CreateSession, Outcome, session_v2::COMPLETION_POLICY_VERSION,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

/// Criteria and configuration fixed for one attempt when it was admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionAdmission {
    attempt_id: String,
    configuration_digest: [u8; 32],
    checks: Vec<CompletionCheck>,
    criteria_digest: [u8; 32],
}
impl CompletionAdmission {
    pub fn new(
        attempt_id: impl Into<String>,
        configuration_digest: [u8; 32],
        checks: Vec<CompletionCheck>,
    ) -> Result<Self> {
        let criteria_digest = criteria_digest(&checks)?;
        Ok(Self {
            attempt_id: attempt_id.into(),
            configuration_digest,
            checks,
            criteria_digest,
        })
    }
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }
    pub fn configuration_digest(&self) -> [u8; 32] {
        self.configuration_digest
    }
    pub fn checks(&self) -> &[CompletionCheck] {
        &self.checks
    }
    pub fn criteria_digest(&self) -> [u8; 32] {
        self.criteria_digest
    }
}

/// Result of one admitted check, as observed by the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriterionStatus {
    Passed,
    Failed,
    /// The check could not be evaluated, so it proves nothing either way.
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriterionResult {
    pub id: String,
    pub status: CriterionStatus,
    /// Fixed detail code such as `exit_nonzero` or `timed_out`; never free text.
    pub detail: &'static str,
}

/// Check results the runtime recorded under an attempt fence, with the binding
/// they were evaluated against. Any mismatch at finalization makes them stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionEvidence {
    pub policy_version: String,
    pub attempt_id: String,
    pub configuration_digest: [u8; 32],
    pub criteria_digest: [u8; 32],
    pub work_revision: u64,
    pub results: Vec<CriterionResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecisionBasis {
    Verified,
    AdapterDowngrade,
    Default,
}
impl DecisionBasis {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::AdapterDowngrade => "adapter_downgrade",
            Self::Default => "default",
        }
    }
}

/// A stored decision row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionDecision {
    pub policy_version: String,
    pub session_id: String,
    pub attempt_id: String,
    pub configuration_digest: [u8; 32],
    pub criteria_digest: [u8; 32],
    pub work_revision: u64,
    pub event_sequence: u64,
    pub checkpoint_id: Option<String>,
    pub outcome: Outcome,
    pub basis: String,
    pub cause: String,
    pub criterion_results: Value,
    pub committed_ms: i64,
}

/// What the adapter proposed and why; neither is evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Proposal {
    Adapter(Outcome),
    PermissionStop,
}

/// Per-store staging for the attempt this connection owns. Cleared when a
/// terminal commits, so a failed terminal write can be retried.
#[derive(Clone, Default)]
pub(crate) struct CompletionStage {
    admission: Option<CompletionAdmission>,
    evidence: Option<CompletionEvidence>,
}

impl CompletionStage {
    pub(crate) fn input(&self, fence: &AttemptFence, proposal: Proposal) -> DecisionInput<'_> {
        DecisionInput {
            proposal,
            admission: self
                .admission
                .as_ref()
                .filter(|admission| admission.attempt_id == fence.attempt_id),
            evidence: self.evidence.as_ref(),
            checkpoint_id: None,
        }
    }
}

pub(crate) struct DecisionInput<'a> {
    pub proposal: Proposal,
    pub admission: Option<&'a CompletionAdmission>,
    pub evidence: Option<&'a CompletionEvidence>,
    pub checkpoint_id: Option<&'a str>,
}

impl Store {
    /// Fix the admitted criteria for the fenced attempt. A second admission for
    /// the same attempt is refused, so the set cannot change after checks ran.
    pub fn admit_completion(
        &mut self,
        fence: &AttemptFence,
        admission: CompletionAdmission,
    ) -> Result<()> {
        check_fence(&self.connection, fence)?;
        if admission.attempt_id != fence.attempt_id {
            return Err(conflict("completion admission belongs to another attempt"));
        }
        if self
            .completion
            .admission
            .as_ref()
            .is_some_and(|current| current.attempt_id == fence.attempt_id)
        {
            return Err(conflict("completion criteria are already admitted"));
        }
        self.completion = CompletionStage {
            admission: Some(admission),
            evidence: None,
        };
        Ok(())
    }

    /// Admitted criteria for the fenced attempt, if any were staged here.
    pub fn completion_admission(&self, fence: &AttemptFence) -> Option<&CompletionAdmission> {
        self.completion
            .admission
            .as_ref()
            .filter(|admission| admission.attempt_id == fence.attempt_id)
    }

    /// Record runtime check results under the live fence. They are rechecked
    /// against the binding when the terminal transaction commits.
    pub fn record_completion_evidence(
        &mut self,
        fence: &AttemptFence,
        evidence: CompletionEvidence,
    ) -> Result<()> {
        check_fence(&self.connection, fence)?;
        self.completion.evidence = Some(evidence);
        Ok(())
    }

    /// Criteria declared when the session was created.
    pub fn session_completion(
        &self,
        session_id: &str,
    ) -> Result<Option<foundry_contracts::CompletionCriteria>> {
        session_criteria(&self.connection, session_id)
    }

    pub fn completion_decision(
        &self,
        session_id: &str,
        attempt_id: &str,
    ) -> Result<Option<CompletionDecision>> {
        read(
            &self.connection,
            "session_id=?1 AND attempt_id=?2",
            params![session_id, attempt_id],
        )
    }

    /// Decision behind the session's current stored outcome. `None` when the
    /// session has no outcome or its outcome predates the policy.
    pub fn current_completion_decision(
        &self,
        session_id: &str,
    ) -> Result<Option<CompletionDecision>> {
        let session = crate::read_session(&self.connection, session_id)?;
        let Some(outcome) = session.outcome else {
            return Ok(None);
        };
        let decision = read(&self.connection, "session_id=?1", params![session_id])?;
        match decision {
            Some(decision) if decision.outcome != outcome => Err(conflict(
                "stored outcome differs from its completion decision",
            )),
            decision => Ok(decision),
        }
    }
}

pub(crate) fn criteria_digest(checks: &[CompletionCheck]) -> Result<[u8; 32]> {
    let encoded = serde_json::to_vec(checks)?;
    Ok(checkpoint_cut::digest(
        b"foundry.completion.criteria.v1",
        &[COMPLETION_POLICY_VERSION.as_bytes(), &encoded],
    ))
}

pub(crate) fn session_criteria(
    connection: &Connection,
    session_id: &str,
) -> Result<Option<foundry_contracts::CompletionCriteria>> {
    let creation: String = connection
        .query_row(
            "SELECT creation FROM sessions WHERE id=?1",
            [session_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| ContractError::NotFound("session".into()))?;
    Ok(serde_json::from_str::<CreateSession>(&creation)?.completion)
}

fn read(
    connection: &Connection,
    filter: &str,
    parameters: impl rusqlite::Params,
) -> Result<Option<CompletionDecision>> {
    let sql = format!(
        "SELECT policy_version,session_id,attempt_id,configuration_digest,criteria_digest,
         work_revision,event_sequence,checkpoint_id,outcome,basis,cause,criterion_results,
         committed_ms FROM completion_decisions WHERE {filter} ORDER BY rowid DESC LIMIT 1"
    );
    let row = connection
        .query_row(&sql, parameters, |row| {
            let decision = CompletionDecision {
                policy_version: row.get(0)?,
                session_id: row.get(1)?,
                attempt_id: row.get(2)?,
                configuration_digest: row.get(3)?,
                criteria_digest: row.get(4)?,
                work_revision: row.get(5)?,
                event_sequence: row.get(6)?,
                checkpoint_id: row.get(7)?,
                outcome: Outcome::ActionRequired,
                basis: row.get(9)?,
                cause: row.get(10)?,
                criterion_results: Value::Null,
                committed_ms: row.get(12)?,
            };
            Ok((
                decision,
                row.get::<_, String>(8)?,
                row.get::<_, String>(11)?,
            ))
        })
        .optional()?;
    let Some((mut decision, outcome, results)) = row else {
        return Ok(None);
    };
    decision.outcome = serde_json::from_value(Value::String(outcome))?;
    decision.criterion_results = serde_json::from_str(&results)?;
    Ok(Some(decision))
}
