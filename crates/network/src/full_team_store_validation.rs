//! Validate every sealed record; never replay new-entry name rules on final state.
use super::*;
use crate::{
    proof::ProofUse,
    signing::{MemberRecord, RecordBody, SignedRecord},
};
use iroh::EndpointId;
use std::collections::BTreeSet;
fn names(body: &MemberRecord) -> bool {
    [&body.person, &body.machine].iter().all(|value| {
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    })
}
fn record<T: RecordBody>(
    value: &SignedRecord<T>,
    document: &TeamDocument,
    policy: &StorePolicy,
    trusted: &BTreeSet<EndpointId>,
    timestamp: i64,
    accepted: i64,
) -> Result<()> {
    if !trusted.contains(&value.by) || timestamp > accepted.saturating_add(86_400_000) {
        return Err(StoreError::Corrupt);
    }
    value
        .verify(&document.id, &policy.profile)
        .map_err(|_| StoreError::Corrupt)
}
pub(super) fn validate(checkpoint: &Checkpoint, policy: &StorePolicy, now: i64) -> Result<()> {
    intents::validate(checkpoint, policy)?;
    if checkpoint.accepted_ms < checkpoint.operation_ms {
        return Err(StoreError::Corrupt);
    }
    if let StoredState::Removed { team, .. } = &checkpoint.state {
        return if checkpoint.revision > 0 && !team.is_empty() {
            Ok(())
        } else {
            Err(StoreError::Corrupt)
        };
    }
    let Some(document) = checkpoint.document() else {
        return if checkpoint.revision == 0
            && checkpoint.parent == [0; 32]
            && checkpoint.accepted_ms == 0
            && checkpoint.operation_ms == 0
        {
            Ok(())
        } else {
            Err(StoreError::Corrupt)
        };
    };
    if checkpoint.revision == 0
        || document.id.is_empty()
        || document.admissions.len()
            + document.members.len()
            + document.invites.len()
            + document.attestations.len()
            > 4096
    {
        return Err(StoreError::Corrupt);
    }
    let mut trusted = BTreeSet::new();
    for (id, value) in &document.admissions {
        if id != &value.body.endpoint.to_string()
            || !value.body.active
            || !names(&value.body)
            || value.body.ts > checkpoint.accepted_ms.saturating_add(86_400_000)
        {
            return Err(StoreError::Corrupt);
        }
        value
            .verify(&document.id, &policy.profile)
            .map_err(|_| StoreError::Corrupt)?;
    }
    loop {
        let before = trusted.len();
        for value in document.admissions.values() {
            if (value.by == document.founder && value.body.endpoint == document.founder)
                || (value.by != value.body.endpoint && trusted.contains(&value.by))
            {
                trusted.insert(value.body.endpoint);
            }
        }
        if before == trusted.len() {
            break;
        }
    }
    if !trusted.contains(&document.founder) || trusted.len() != document.admissions.len() {
        return Err(StoreError::Corrupt);
    }
    for (id, value) in &document.members {
        if id != &value.body.endpoint.to_string() || !names(&value.body) {
            return Err(StoreError::Corrupt);
        }
        record(
            value,
            document,
            policy,
            &trusted,
            value.body.ts,
            checkpoint.accepted_ms,
        )?;
    }
    record(
        &document.meta,
        document,
        policy,
        &trusted,
        document.meta.body.ts,
        checkpoint.accepted_ms,
    )?;
    for (id, value) in &document.invites {
        if id != &value.body.hash {
            return Err(StoreError::Corrupt);
        }
        record(
            value,
            document,
            policy,
            &trusted,
            value.body.ts,
            checkpoint.accepted_ms,
        )?;
    }
    if let Some(value) = &document.link {
        record(
            value,
            document,
            policy,
            &trusted,
            value.body.ts,
            checkpoint.accepted_ms,
        )?;
    }
    if !trusted.contains(&checkpoint.identity)
        || !document
            .members
            .get(&checkpoint.identity.to_string())
            .is_some_and(|m| m.body.active)
    {
        return Err(StoreError::Unsupported);
    }
    for (id, token) in &document.attestations {
        if !document.members.contains_key(id) {
            return Err(StoreError::Corrupt);
        }
        let endpoint = id.parse().map_err(|_| StoreError::Corrupt)?;
        policy
            .verifier
            .as_ref()
            .ok_or(StoreError::Unsupported)?
            .verify(&endpoint, token, ProofUse::Ownership, now / 1000)
            .map_err(|_| StoreError::Corrupt)?;
    }
    Ok(())
}
