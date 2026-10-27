//! In-memory full-team transitions. Returned candidates are not durable commits.
//! Proofs require an explicitly configured concrete verifier. No durable store
//! or CLI integration is supplied by this engine.
#[path = "team_state_merge.rs"]
mod merge;
#[path = "team_state_operations.rs"]
mod operations;
#[path = "team_state_proofs.rs"]
mod proofs;
use crate::proof::{ProofUse, ProofVerifier};
use crate::{
    WireProfile,
    membership::{MembershipSnapshot, MembershipState, MergeReport},
    signing::{MemberRecord, MetadataRecord, SignedRecord},
    team::{InviteRecord, LinkRecord, TeamDocument},
};
use iroh::{EndpointId, SecretKey};
use std::{collections::BTreeMap, fmt, sync::Arc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamError {
    Invalid,
    Unauthorized,
    DifferentTeam,
    ProofVerificationUnavailable,
    InvalidProof,
    UnknownInvite,
    InactiveInviter,
    InviteUsed,
    InviteExpired,
    LinkReplaced,
    WrongPerson,
    OwnerRequired,
    NameTaken,
    Entropy,
    ClockOverflow,
    InvalidState,
}
impl fmt::Display for TeamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid team operation",
            Self::Unauthorized => "team actor is not active",
            Self::DifferentTeam => "different team identity",
            Self::ProofVerificationUnavailable => "team proof verification unavailable",
            Self::InvalidProof => "invalid team proof",
            Self::UnknownInvite => "unknown invitation",
            Self::InactiveInviter => "invitation signer is inactive",
            Self::InviteUsed => "invitation already consumed",
            Self::InviteExpired => "invitation expired",
            Self::LinkReplaced => "team link replaced",
            Self::WrongPerson => "invitation person mismatch",
            Self::OwnerRequired => "same-person admitting machine required",
            Self::NameTaken => "person name already held",
            Self::Entropy => "team randomness unavailable",
            Self::ClockOverflow => "team timestamp overflow",
            Self::InvalidState => "team state verification failed",
        })
    }
}
impl std::error::Error for TeamError {}
pub type Result<T> = std::result::Result<T, TeamError>;

/// Trusted entropy injection for reproducible tests; production uses SystemEntropy.
pub trait Entropy {
    fn fill(&mut self, bytes: &mut [u8]) -> Result<()>;
}
pub struct SystemEntropy;
impl Entropy for SystemEntropy {
    fn fill(&mut self, bytes: &mut [u8]) -> Result<()> {
        getrandom::fill(bytes).map_err(|_| TeamError::Entropy)
    }
}
pub struct Formation<'a> {
    pub name: &'a str,
    pub person: &'a str,
    pub machine: &'a str,
}
/// Endpoint must come from the authenticated join connection, never request JSON.
pub struct Admission<'a> {
    pub endpoint: EndpointId,
    pub person: &'a str,
    pub machine: &'a str,
    pub secret: &'a str,
    pub proof: Option<&'a str>,
    pub owner_only: bool,
}

#[derive(Clone)]
pub struct TeamState {
    document: TeamDocument,
    membership: MembershipState,
    profile: WireProfile,
    verifier: Option<Arc<ProofVerifier>>,
}
impl TeamState {
    pub(crate) fn restore_verified(
        snapshot: &crate::full_team_store::VerifiedLocalSnapshot<'_>,
    ) -> Result<Self> {
        let membership = MembershipState::restore_verified(snapshot);
        if membership.snapshot() != &projection(snapshot.document()) {
            return Err(TeamError::InvalidState);
        }
        Ok(Self {
            document: snapshot.document().clone(),
            membership,
            profile: snapshot.profile().clone(),
            verifier: snapshot.verifier(),
        })
    }
    pub fn found(
        fields: Formation<'_>,
        key: &SecretKey,
        profile: &WireProfile,
        now_ms: i64,
        entropy: &mut impl Entropy,
    ) -> Result<Self> {
        if fields.name.trim().is_empty() || !name(fields.person) || !name(fields.machine) {
            return Err(TeamError::Invalid);
        }
        profile.validate().map_err(|_| TeamError::Invalid)?;
        let id = random_secret(entropy)?;
        let member = SignedRecord::sign(
            MemberRecord {
                endpoint: key.public(),
                person: fields.person.into(),
                machine: fields.machine.into(),
                active: true,
                ts: next_ts(0, now_ms),
            },
            key,
            &id,
            profile,
        )
        .map_err(|_| TeamError::Invalid)?;
        let document = TeamDocument {
            id: id.clone(),
            founder: key.public(),
            meta: SignedRecord::sign(
                MetadataRecord {
                    name: fields.name.trim().into(),
                    relay: None,
                    ts: now_ms,
                },
                key,
                &id,
                profile,
            )
            .map_err(|_| TeamError::Invalid)?,
            admissions: BTreeMap::from([(key.public().to_string(), member.clone())]),
            members: BTreeMap::from([(key.public().to_string(), member)]),
            invites: BTreeMap::new(),
            link: None,
            attestations: BTreeMap::new(),
        };
        let (state, _) = Self::bootstrap(&id, key.public(), &document, profile, now_ms)?;
        state.rotate_link(key, now_ms, entropy)
    }
    /// Caller supplies trusted expected identity. This does not prove freshness or
    /// local admission. Unsupported proof-bearing input is never silently cleared.
    pub fn bootstrap(
        expected_team: &str,
        expected_founder: EndpointId,
        incoming: &TeamDocument,
        profile: &WireProfile,
        now_ms: i64,
    ) -> Result<(Self, MergeReport)> {
        Self::bootstrap_configured(
            expected_team,
            expected_founder,
            incoming,
            profile,
            now_ms,
            None,
        )
    }
    /// Trust configuration is selected by the caller, never the incoming document.
    pub fn bootstrap_configured(
        expected_team: &str,
        expected_founder: EndpointId,
        incoming: &TeamDocument,
        profile: &WireProfile,
        now_ms: i64,
        verifier: Option<Arc<ProofVerifier>>,
    ) -> Result<(Self, MergeReport)> {
        if verifier.is_none() {
            no_proofs(incoming)?;
        }
        if incoming.id != expected_team || incoming.founder != expected_founder {
            return Err(TeamError::DifferentTeam);
        }
        let mut verified_document = incoming.clone();
        verified_document.attestations.clear();
        let mut proof_report = MergeReport::default();
        proofs::merge_attestations(
            &mut verified_document,
            incoming,
            verifier.as_deref(),
            now_ms,
            &mut proof_report,
        )?;
        let owners = proofs::ownership(&verified_document, verifier.as_deref(), now_ms);
        let (membership, mut report) = MembershipState::bootstrap_with_ownership(
            expected_team,
            expected_founder,
            &projection(incoming),
            profile,
            now_ms,
            &owners,
        )
        .map_err(|_| TeamError::InvalidState)?;
        let mut state = Self {
            document: verified_document,
            membership,
            profile: profile.clone(),
            verifier,
        };
        state.document.invites.clear();
        state.document.link = None;
        state.sync_membership();
        state.prune_proofs();
        report.rejected += proof_report.rejected;
        state.merge_secrets(incoming, now_ms, &mut report);
        Ok((state, report))
    }
    /// Privileged access: this document contains bearer secrets. Never log it.
    pub fn document(&self) -> &TeamDocument {
        &self.document
    }
    pub fn is_active(&self, endpoint: EndpointId) -> bool {
        self.membership.is_active(endpoint)
    }
    /// Unknown peers receive nothing. Known inactive peers get roster state with
    /// every invitation and the shared link removed, even before disconnection.
    pub fn document_for(&self, peer: EndpointId) -> Option<TeamDocument> {
        if !self.document.members.contains_key(&peer.to_string()) {
            return None;
        }
        let mut result = self.document.clone();
        if !self.is_active(peer) {
            result.link = None;
            result.invites.clear();
        }
        Some(result)
    }
    fn actor(&self, key: &SecretKey) -> Result<()> {
        if self.is_active(key.public()) {
            Ok(())
        } else {
            Err(TeamError::Unauthorized)
        }
    }
    fn trusted(&self, signer: EndpointId) -> bool {
        self.membership
            .snapshot()
            .admissions
            .contains_key(&signer.to_string())
    }
    fn sync_membership(&mut self) {
        let snapshot = self.membership.snapshot();
        self.document.meta = snapshot.metadata.clone();
        self.document.admissions = snapshot.admissions.clone();
        self.document.members = snapshot.members.clone();
    }
    fn refresh(&mut self, actor: EndpointId, now_ms: i64) -> Result<()> {
        let snapshot = projection(&self.document);
        // Apply to known history, not an empty bootstrap: name vouching can depend
        // on members that later depart, while their existing siblings stay active.
        let report = self
            .membership
            .merge_from(actor, &snapshot, &self.profile, now_ms)
            .map_err(|_| TeamError::InvalidState)?;
        if report.rejected != 0
            || report.proof_required != 0
            || self.membership.snapshot() != &snapshot
        {
            return Err(TeamError::InvalidState);
        }
        Ok(())
    }
    fn put_member(
        &mut self,
        key: &SecretKey,
        endpoint: EndpointId,
        person: &str,
        machine: &str,
        active: bool,
        now_ms: i64,
    ) -> Result<()> {
        let previous = self
            .document
            .members
            .get(&endpoint.to_string())
            .map_or(0, |r| r.body.ts);
        let record = SignedRecord::sign(
            MemberRecord {
                endpoint,
                person: person.into(),
                machine: machine.into(),
                active,
                ts: next_ts(previous, now_ms),
            },
            key,
            &self.document.id,
            &self.profile,
        )
        .map_err(|_| TeamError::Invalid)?;
        if active {
            self.document
                .admissions
                .entry(endpoint.to_string())
                .or_insert_with(|| record.clone());
        }
        self.document.members.insert(endpoint.to_string(), record);
        Ok(())
    }
}
fn projection(document: &TeamDocument) -> MembershipSnapshot {
    MembershipSnapshot {
        team: document.id.clone(),
        founder: document.founder,
        metadata: document.meta.clone(),
        admissions: document.admissions.clone(),
        members: document.members.clone(),
    }
}
fn no_proofs(document: &TeamDocument) -> Result<()> {
    if document.attestations.is_empty() {
        Ok(())
    } else {
        Err(TeamError::ProofVerificationUnavailable)
    }
}
fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn next_ts(previous: i64, now: i64) -> i64 {
    previous.max(now.saturating_sub(1)).saturating_add(1)
}
fn random_secret(entropy: &mut impl Entropy) -> Result<String> {
    let mut bytes = [0; 16];
    entropy.fill(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn hash(secret: &str) -> String {
    blake3::hash(secret.as_bytes()).to_hex().to_string()
}
