//! Shared fixtures for the foundry.completion.v1 store tests.
#![allow(dead_code, unused_imports)]
pub use foundry_contracts::{
    CancelMode, CompletionCheck, CompletionCriteria, ContractError, CreateSession, Lifecycle,
    Outcome, session_v2::COMPLETION_POLICY_VERSION,
};
pub use foundry_store::{
    AttemptFence, CompletionAdmission, CompletionEvidence, CriterionResult, CriterionStatus, Store,
    StoreError,
};
pub use rusqlite::Connection;
pub use serde_json::json;
pub use tempfile::{TempDir, tempdir};

pub const CONFIGURATION: [u8; 32] = [7; 32];
pub const V11: &str = include_str!("../fixtures/store-v11.sql");
pub const PRE_POLICY_EXIT: &str = "519757b5-24eb-4e9c-93cf-f7387e1edf08";

pub fn check(id: &str) -> CompletionCheck {
    CompletionCheck::File {
        id: id.into(),
        path: format!("{id}.txt"),
        sha256: None,
    }
}

pub fn open(checks: Vec<CompletionCheck>) -> (TempDir, Store, String) {
    let dir = tempdir().unwrap();
    let mut store = Store::open(dir.path().join("db")).unwrap();
    let completion = (!checks.is_empty()).then_some(CompletionCriteria {
        profile: None,
        checks,
    });
    let session = store
        .create(CreateSession {
            prompt: "task".into(),
            directory: dir.path().display().to_string(),
            completion,
            ..Default::default()
        })
        .unwrap();
    (dir, store, session.id)
}

pub fn admit(store: &mut Store, id: &str) -> (AttemptFence, CompletionAdmission) {
    let version = store.get(id).unwrap().version;
    let fence = store.begin_attempt(id, version).unwrap();
    let checks = store.session_completion(id).unwrap().unwrap().checks;
    let admission = CompletionAdmission::new(&fence.attempt_id, CONFIGURATION, checks).unwrap();
    store.admit_completion(&fence, admission.clone()).unwrap();
    (fence, admission)
}

/// Evidence as the runtime records it: bound to the admission and current work.
pub fn evidence(
    store: &Store,
    session: &str,
    admission: &CompletionAdmission,
    statuses: &[(&str, CriterionStatus)],
) -> CompletionEvidence {
    CompletionEvidence {
        policy_version: COMPLETION_POLICY_VERSION.into(),
        attempt_id: admission.attempt_id().into(),
        configuration_digest: admission.configuration_digest(),
        criteria_digest: admission.criteria_digest(),
        work_revision: store.work_revision(session).unwrap(),
        results: statuses
            .iter()
            .map(|(id, status)| CriterionResult {
                id: (*id).into(),
                status: *status,
                detail: "fixture",
            })
            .collect(),
    }
}

pub fn success(store: &mut Store, fence: &AttemptFence) -> foundry_contracts::Session {
    store
        .finish_attempt(
            fence,
            Lifecycle::Exit,
            Some(Outcome::Success),
            Some("done".into()),
        )
        .unwrap()
}

pub fn cause(store: &Store, fence: &AttemptFence) -> (String, String) {
    let decision = store
        .completion_decision(&fence.session_id, &fence.attempt_id)
        .unwrap()
        .unwrap();
    (decision.basis, decision.cause)
}
