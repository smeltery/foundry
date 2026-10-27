use super::*;
use crate::proof::VerifiedProof;
impl TeamState {
    /// Select trusted policy once, before attaching proofs. No peer-supplied policy.
    pub fn with_proof_verifier(mut self, verifier: Arc<ProofVerifier>) -> Result<Self> {
        if self.verifier.is_some() || !self.document.attestations.is_empty() {
            return Err(TeamError::InvalidProof);
        }
        self.verifier = Some(verifier);
        Ok(self)
    }
    pub(super) fn verify_request<'a>(
        &'a self,
        endpoint: &EndpointId,
        token: &str,
        now: i64,
    ) -> Result<VerifiedProof<'a>> {
        self.verifier
            .as_ref()
            .ok_or(TeamError::ProofVerificationUnavailable)?
            .verify(endpoint, token, ProofUse::Ownership, now / 1000)
            .map_err(|_| TeamError::InvalidProof)
    }
    /// Explicit private-state attachment; invalid input never becomes proof-free success.
    pub fn attach_proof(&self, endpoint: EndpointId, token: &str, now: i64) -> Result<Self> {
        self.verify_request(&endpoint, token, now)?;
        if !self.document.members.contains_key(&endpoint.to_string()) {
            return Err(TeamError::Invalid);
        }
        let mut next = self.clone();
        replace(
            &mut next.document,
            endpoint,
            token,
            self.verifier.as_deref().unwrap(),
            now,
        )?;
        Ok(next)
    }
    pub fn has_current_proof(&self, endpoint: EndpointId, now: i64) -> bool {
        let Some(member) = self.document.members.get(&endpoint.to_string()) else {
            return false;
        };
        let Some(token) = self.document.attestations.get(&endpoint.to_string()) else {
            return false;
        };
        self.verifier
            .as_ref()
            .and_then(|verifier| {
                verifier
                    .verify(&endpoint, token, ProofUse::Current, now / 1000)
                    .ok()
            })
            .is_some_and(|proof| proof.person() == member.body.person)
    }
    pub fn verified_people(&self, now: i64) -> Vec<String> {
        let mut people: Vec<_> = self
            .document
            .members
            .values()
            .filter(|member| {
                self.is_active(member.body.endpoint)
                    && self.has_current_proof(member.body.endpoint, now)
            })
            .map(|member| member.body.person.clone())
            .collect();
        people.sort();
        people.dedup();
        people
    }
    pub(super) fn same_account(
        &self,
        person: &str,
        endpoint: EndpointId,
        proof: &VerifiedProof<'_>,
        now: i64,
    ) -> bool {
        if proof.person() != person {
            return false;
        }
        ownership(&self.document, self.verifier.as_deref(), now)
            .iter()
            .any(|(other, (subject, name))| {
                *other != endpoint
                    && self.is_active(*other)
                    && subject == proof.subject()
                    && name == person
                    && self
                        .document
                        .members
                        .get(&other.to_string())
                        .is_some_and(|m| m.body.person == person)
            })
    }
    pub(super) fn prune_proofs(&mut self) {
        self.document
            .attestations
            .retain(|id, _| self.document.members.contains_key(id));
    }
}
pub(super) fn ownership(
    document: &TeamDocument,
    verifier: Option<&ProofVerifier>,
    now: i64,
) -> BTreeMap<EndpointId, (String, String)> {
    let Some(verifier) = verifier else {
        return BTreeMap::new();
    };
    document
        .attestations
        .iter()
        .filter_map(|(id, token)| {
            let endpoint = id.parse().ok()?;
            let proof = verifier
                .verify(&endpoint, token, ProofUse::Ownership, now / 1000)
                .ok()?;
            Some((
                endpoint,
                (proof.subject().to_owned(), proof.person().to_owned()),
            ))
        })
        .collect()
}
fn replace(
    document: &mut TeamDocument,
    endpoint: EndpointId,
    token: &str,
    verifier: &ProofVerifier,
    now: i64,
) -> Result<bool> {
    let candidate = verifier
        .verify(&endpoint, token, ProofUse::Ownership, now / 1000)
        .map_err(|_| TeamError::InvalidProof)?;
    let id = endpoint.to_string();
    if document
        .attestations
        .get(&id)
        .and_then(|old| {
            verifier
                .verify(&endpoint, old, ProofUse::Ownership, now / 1000)
                .ok()
        })
        .is_some_and(|old| old.replacement_key() >= candidate.replacement_key())
    {
        return Ok(false);
    }
    document
        .attestations
        .insert(id, candidate.token_for_storage().to_owned());
    Ok(true)
}
pub(super) fn merge_attestations(
    document: &mut TeamDocument,
    incoming: &TeamDocument,
    verifier: Option<&ProofVerifier>,
    now: i64,
    report: &mut MergeReport,
) -> Result<()> {
    let Some(verifier) = verifier else {
        no_proofs(document)?;
        return no_proofs(incoming);
    };
    for (id, token) in &incoming.attestations {
        if !document.members.contains_key(id) && !incoming.members.contains_key(id) {
            continue;
        }
        let result = id
            .parse()
            .map_err(|_| TeamError::InvalidProof)
            .and_then(|endpoint| replace(document, endpoint, token, verifier, now));
        match result {
            Ok(changed) => report.changed |= changed,
            Err(_) => report.rejected += 1,
        }
    }
    Ok(())
}
