//! Admission and member-record projection, not the full replicated team document.
//! Hosted proofs, invitations and link state require separate verified adapters.
#[path = "team_state_membership.rs"]
mod team_integration;
use crate::signing::{MemberRecord, MetadataRecord, SignedRecord};
use crate::{NetworkError, Result, WireProfile};
use iroh::EndpointId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A deliberately distinct projection: full state cannot silently lose other fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipSnapshot {
    pub team: String,
    pub founder: EndpointId,
    pub metadata: SignedRecord<MetadataRecord>,
    #[serde(default)]
    pub admissions: BTreeMap<String, SignedRecord<MemberRecord>>,
    pub members: BTreeMap<String, SignedRecord<MemberRecord>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeReport {
    pub changed: bool,
    /// Invalid or unauthorized new entries; old/duplicate entries are not failures.
    pub rejected: usize,
    /// Conflicting person names need a hosted-proof adapter or same-person signer.
    pub proof_required: usize,
}

#[derive(Debug, Clone)]
pub struct MembershipState {
    snapshot: MembershipSnapshot,
    signing_prefix: String,
}

impl MembershipState {
    /// Verify a join snapshot against an independently trusted team/founder pair.
    /// A valid snapshot alone does not authorize admission of the local endpoint.
    pub fn bootstrap(
        expected_team: &str,
        expected_founder: EndpointId,
        incoming: &MembershipSnapshot,
        profile: &WireProfile,
        now_ms: i64,
    ) -> Result<(Self, MergeReport)> {
        Self::bootstrap_with_ownership(
            expected_team,
            expected_founder,
            incoming,
            profile,
            now_ms,
            &BTreeMap::new(),
        )
    }

    pub(crate) fn bootstrap_with_ownership(
        expected_team: &str,
        expected_founder: EndpointId,
        incoming: &MembershipSnapshot,
        profile: &WireProfile,
        now_ms: i64,
        owners: &BTreeMap<EndpointId, (String, String)>,
    ) -> Result<(Self, MergeReport)> {
        if incoming.team != expected_team || incoming.founder != expected_founder {
            return Err(NetworkError::Invalid("unexpected team identity"));
        }
        profile.validate()?;
        let mut state = Self {
            signing_prefix: profile.signing_prefix.clone(),
            snapshot: MembershipSnapshot {
                team: incoming.team.clone(),
                founder: expected_founder,
                metadata: incoming.metadata.clone(),
                admissions: BTreeMap::new(),
                members: BTreeMap::new(),
            },
        };
        let report = state.absorb(incoming, profile, now_ms, owners);
        let trusted = state.depths();
        if !trusted.contains_key(&expected_founder)
            || !trusted.contains_key(&state.snapshot.metadata.by)
            || !plausible(state.snapshot.metadata.body.ts, now_ms)
        {
            return Err(NetworkError::Invalid("untrusted founding state"));
        }
        state.snapshot.metadata.verify(expected_team, profile)?;
        Ok((state, report))
    }

    pub fn snapshot(&self) -> &MembershipSnapshot {
        &self.snapshot
    }

    pub fn is_active(&self, endpoint: EndpointId) -> bool {
        self.snapshot
            .members
            .get(&endpoint.to_string())
            .is_some_and(|record| record.body.active)
            && self.depths().contains_key(&endpoint)
    }

    /// Pass the Iroh connection's authenticated remote_id(), never a JSON claim.
    /// Existing active membership is required before examining incoming admissions.
    pub fn merge_from(
        &mut self,
        authenticated_peer: EndpointId,
        incoming: &MembershipSnapshot,
        profile: &WireProfile,
        now_ms: i64,
    ) -> Result<MergeReport> {
        self.merge_with_ownership(
            authenticated_peer,
            incoming,
            profile,
            now_ms,
            &BTreeMap::new(),
        )
    }

    pub(crate) fn merge_with_ownership(
        &mut self,
        authenticated_peer: EndpointId,
        incoming: &MembershipSnapshot,
        profile: &WireProfile,
        now_ms: i64,
        owners: &BTreeMap<EndpointId, (String, String)>,
    ) -> Result<MergeReport> {
        if profile.signing_prefix != self.signing_prefix {
            return Err(NetworkError::Invalid("changed signing domain"));
        }
        if incoming.team != self.snapshot.team || incoming.founder != self.snapshot.founder {
            return Err(NetworkError::Invalid("different team identity"));
        }
        if !self.is_active(authenticated_peer) {
            return Err(NetworkError::Invalid("peer is not an active member"));
        }
        profile.validate()?;
        Ok(self.absorb(incoming, profile, now_ms, owners))
    }

    fn depths(&self) -> BTreeMap<EndpointId, usize> {
        let mut result = BTreeMap::new();
        loop {
            let before = result.len();
            for record in self.snapshot.admissions.values() {
                let endpoint = record.body.endpoint;
                if result.contains_key(&endpoint) {
                    continue;
                }
                let depth = if record.by == endpoint {
                    (endpoint == self.snapshot.founder).then_some(0)
                } else {
                    result.get(&record.by).map(|depth| depth + 1)
                };
                if let Some(depth) = depth {
                    result.insert(endpoint, depth);
                }
            }
            if result.len() == before {
                return result;
            }
        }
    }

    fn absorb(
        &mut self,
        incoming: &MembershipSnapshot,
        profile: &WireProfile,
        now_ms: i64,
        owners: &BTreeMap<EndpointId, (String, String)>,
    ) -> MergeReport {
        let mut report = MergeReport::default();
        let mut pending: Vec<_> = incoming
            .admissions
            .iter()
            .filter(|(id, _)| !self.snapshot.admissions.contains_key(*id))
            .collect();
        loop {
            let trusted = self.depths();
            let before = pending.len();
            pending.retain(|(id, record)| {
                let body = &record.body;
                let authorized = if record.by == body.endpoint {
                    record.by == self.snapshot.founder
                } else {
                    trusted.contains_key(&record.by)
                };
                if authorized
                    && body.active
                    && *id == &body.endpoint.to_string()
                    && plausible(body.ts, now_ms)
                    && record.verify(&self.snapshot.team, profile).is_ok()
                {
                    self.snapshot
                        .admissions
                        .insert((*id).clone(), (*record).clone());
                    report.changed = true;
                    false
                } else {
                    true
                }
            });
            if pending.len() == before {
                report.rejected += pending.len();
                break;
            }
        }
        let trusted = self.depths();
        let mut members: Vec<_> = incoming.members.iter().collect();
        members.sort_by_key(|(_, record)| {
            trusted
                .get(&record.body.endpoint)
                .copied()
                .unwrap_or(usize::MAX)
        });
        for (id, record) in members {
            let body = &record.body;
            let current = self.snapshot.members.get(id);
            if current.is_some_and(|old| (body.ts, &record.sig) <= (old.body.ts, &old.sig)) {
                continue;
            }
            let self_return = record.by == body.endpoint
                && body.active
                && !(record.by == self.snapshot.founder && current.is_none());
            if id != &body.endpoint.to_string()
                || !valid_names(body)
                || !plausible(body.ts, now_ms)
                || self_return
                || !trusted.contains_key(&record.by)
                || record.verify(&self.snapshot.team, profile).is_err()
            {
                report.rejected += 1;
                continue;
            }
            if body.active
                && !self.name_available(body, record.by, &trusted)
                && !self.proven_owner(body, owners)
            {
                report.proof_required += 1;
                report.rejected += 1;
                continue;
            }
            self.snapshot.members.insert(id.clone(), record.clone());
            report.changed = true;
        }
        let record = &incoming.metadata;
        let old = &self.snapshot.metadata;
        if (record.body.ts, &record.sig) > (old.body.ts, &old.sig) {
            if plausible(record.body.ts, now_ms)
                && trusted.contains_key(&record.by)
                && record.verify(&self.snapshot.team, profile).is_ok()
            {
                self.snapshot.metadata = record.clone();
                report.changed = true;
            } else {
                report.rejected += 1;
            }
        }
        report
    }

    fn name_available(
        &self,
        body: &MemberRecord,
        signer: EndpointId,
        trusted: &BTreeMap<EndpointId, usize>,
    ) -> bool {
        let active: Vec<_> = self
            .snapshot
            .members
            .values()
            .map(|record| &record.body)
            .filter(|member| member.active && trusted.contains_key(&member.endpoint))
            .collect();
        active
            .iter()
            .any(|member| member.endpoint == signer && member.person == body.person)
            || !active
                .iter()
                .any(|member| member.endpoint != body.endpoint && member.person == body.person)
    }
}

fn plausible(timestamp: i64, now_ms: i64) -> bool {
    timestamp <= now_ms.saturating_add(86_400_000)
}
fn valid_names(member: &MemberRecord) -> bool {
    [&member.person, &member.machine].into_iter().all(|name| {
        name.len() <= 64
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    })
}
