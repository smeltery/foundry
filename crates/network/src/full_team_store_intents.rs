//! Local design: signed atomic intent/effect sets and generation fencing. These
//! guarantees are local storage rules, not a wire-compatibility or waiver claim.
use super::*;
use intent_types::StoredIntent;
use std::collections::BTreeSet;
pub(super) const MAX_INTENTS: usize = 128;
const MAX_EFFECTS: usize = 512;
fn active(document: &TeamDocument, endpoint: iroh::EndpointId) -> bool {
    document
        .members
        .get(&endpoint.to_string())
        .is_some_and(|m| m.body.active)
}
pub(super) fn eligible(intent: &StoredIntent, document: &TeamDocument, now: i64) -> bool {
    document.meta.body.relay.is_none()
        && intent.expires_ms > now
        && active(document, intent.creator)
        && intent.person.as_ref().is_none_or(|expected| {
            document
                .members
                .get(&intent.creator.to_string())
                .is_some_and(|member| &member.body.person == expected)
        })
        && document
            .invites
            .get(&intent.invitation_hash)
            .is_some_and(|i| {
                i.by == intent.creator
                    && i.body.used_by.is_none()
                    && !i.body.reusable
                    && i.body.expires == intent.expires_ms
                    && i.body.ts == intent.issued_ms
                    && i.body.person == intent.person
            })
}
pub(super) fn validate(checkpoint: &Checkpoint, policy: &StorePolicy) -> Result<()> {
    if checkpoint.version == 2 {
        return if checkpoint.code_intents.is_empty()
            && checkpoint.effects.is_empty()
            && checkpoint.network_generation == 0
        {
            Ok(())
        } else {
            Err(StoreError::Corrupt)
        };
    }
    let codes = policy.codes.as_ref().ok_or(StoreError::Binding)?;
    if checkpoint.version != 3
        || checkpoint.code_intents.len() > MAX_INTENTS
        || checkpoint.effects.len() > MAX_EFFECTS
        || checkpoint.network_generation > checkpoint.revision
    {
        return Err(StoreError::Corrupt);
    }
    let Some(document) = checkpoint.document() else {
        return if checkpoint.code_intents.is_empty() && checkpoint.effects.is_empty() {
            Ok(())
        } else {
            Err(StoreError::Corrupt)
        };
    };
    if checkpoint.network_generation == 0 {
        return Err(StoreError::Corrupt);
    }
    let mut ids = BTreeSet::new();
    let mut hashes = BTreeSet::new();
    for intent in &checkpoint.code_intents {
        if !ids.insert(intent.id)
            || !hashes.insert(&intent.invitation_hash)
            || intent.creator != checkpoint.identity
            || intent.team != document.id
            || intent.founder != document.founder
            || intent.incarnation != checkpoint.incarnation
            || intent.policy != codes.binding()
            || intent.creation_revision == 0
            || intent.creation_revision > checkpoint.revision
            || intent.issued_ms < 0
            || intent.issued_ms > checkpoint.accepted_ms
            || intent
                .expires_ms
                .checked_sub(intent.issued_ms)
                .is_none_or(|ms| {
                    !(3_600_000..=720 * 3_600_000).contains(&ms) || ms % 3_600_000 != 0
                })
        {
            return Err(StoreError::Corrupt);
        }
        let canonical = codes
            .normalize(&intent.code)
            .map_err(|_| StoreError::Corrupt)?;
        if canonical.expose() != intent.code {
            return Err(StoreError::Corrupt);
        }
        let derived = codes.derive(&canonical).map_err(|_| StoreError::Corrupt)?;
        if data_encoding::HEXLOWER.encode(&derived.invitation_key()) != intent.invitation_hash {
            return Err(StoreError::Corrupt);
        }
        let invitation = document
            .invites
            .get(&intent.invitation_hash)
            .ok_or(StoreError::Corrupt)?;
        if invitation.body.hash != intent.invitation_hash
            || invitation.body.expires != intent.expires_ms
            || invitation.body.reusable
            || invitation.body.person != intent.person
            || invitation.body.ts < intent.issued_ms
            || (invitation.body.used_by.is_none()
                && (invitation.by != intent.creator || invitation.body.ts != intent.issued_ms))
        {
            return Err(StoreError::Corrupt);
        }
    }
    let mut reconciles = BTreeSet::new();
    let mut retires = BTreeSet::new();
    let mut publish = false;
    for effect in &checkpoint.effects {
        let generation = match effect {
            ControlEffect::ReconcileCode { id, generation } => {
                if !ids.contains(id) || !reconciles.insert(*id) {
                    return Err(StoreError::Corrupt);
                }
                *generation
            }
            ControlEffect::RetireCode { id, generation } => {
                if ids.contains(id) || !retires.insert(*id) {
                    return Err(StoreError::Corrupt);
                }
                *generation
            }
            ControlEffect::PublishLatestState {
                revision,
                generation,
            } => {
                if publish || *revision == 0 || *revision > checkpoint.revision {
                    return Err(StoreError::Corrupt);
                }
                publish = true;
                *generation
            }
        };
        if generation != checkpoint.network_generation {
            return Err(StoreError::Corrupt);
        }
    }
    Ok(())
}
/// Supersede queued descriptors with current desired state. Outstanding retirement
/// IDs survive until explicit acknowledgement; accepted intents are never evicted.
pub(super) fn refresh(
    base: &Checkpoint,
    next: &mut Checkpoint,
    policy: &StorePolicy,
    now: i64,
    document_changed: bool,
) -> Result<()> {
    if policy.codes.is_none() {
        return Ok(());
    }
    next.network_generation = base.network_generation;
    let Some(document) = next.document() else {
        if document_changed {
            next.network_generation = base
                .network_generation
                .checked_add(1)
                .ok_or(StoreError::RevisionOverflow)?;
        }
        return Ok(());
    };
    let mut retired: BTreeSet<_> = base
        .effects
        .iter()
        .filter_map(|e| match e {
            ControlEffect::RetireCode { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    let mut intents = Vec::new();
    for intent in &base.code_intents {
        if eligible(intent, document, now) {
            intents.push(intent.clone());
        } else {
            retired.insert(intent.id);
        }
    }
    let changed = document_changed || intents != base.code_intents;
    if !changed {
        next.code_intents = intents;
        next.effects = base.effects.clone();
        return Ok(());
    }
    next.network_generation = base
        .network_generation
        .checked_add(1)
        .ok_or(StoreError::RevisionOverflow)?;
    let generation = next.network_generation;
    let mut effects: Vec<_> = retired
        .into_iter()
        .map(|id| ControlEffect::RetireCode { id, generation })
        .collect();
    effects.extend(intents.iter().map(|i| ControlEffect::ReconcileCode {
        id: i.id,
        generation,
    }));
    effects.push(ControlEffect::PublishLatestState {
        revision: base
            .revision
            .checked_add(1)
            .ok_or(StoreError::RevisionOverflow)?,
        generation,
    });
    if effects.len() > MAX_EFFECTS {
        return Err(StoreError::TooLarge);
    }
    next.code_intents = intents;
    next.effects = effects;
    Ok(())
}
impl FullTeamStore {
    /// Generates once. A conflict requires reload and explicit reprepare; there is
    /// no public code or endpoint material on the precommit capability.
    pub fn prepare_code_intent(
        &self,
        base: &LoadedTeam,
        hours: u64,
        own: bool,
        now: i64,
    ) -> Result<PendingCodeIntent> {
        let codes = self.policy.codes.as_ref().ok_or(StoreError::Unsupported)?;
        if now < 0 {
            return Err(StoreError::Invalid);
        }
        let state = self.engine(base, now)?;
        let code = codes.generate().map_err(|_| StoreError::Invalid)?;
        let derived = codes.derive(&code).map_err(|_| StoreError::Invalid)?;
        let hash = data_encoding::HEXLOWER.encode(&derived.invitation_key());
        if state.document().invites.contains_key(&hash) {
            return Err(StoreError::Conflict);
        }
        let state = state
            .issue_invite(
                self.identity.secret_key(),
                derived.bearer_secret(),
                hours,
                own,
                now,
            )
            .map_err(|_| StoreError::Invalid)?;
        let invitation = &state.document().invites[&hash];
        let expires_ms = invitation.body.expires;
        let ticket = crate::ticket::Ticket {
            team: state.document().id.clone(),
            secret: derived.bearer_secret().into(),
            name: state.document().meta.body.name.clone(),
            relay: state.document().meta.body.relay.clone(),
            hint_relay: None,
            hint_ips: Vec::new(),
            peers: vec![self.identity.endpoint_id()],
        };
        let ticket = crate::ticket::TicketCodec::new(&self.policy.profile, None)
            .and_then(|codec| codec.encode(&ticket))
            .map_err(|_| StoreError::Invalid)?;
        let private_relay = state.document().meta.body.relay.is_some();
        let person = invitation.body.person.clone();
        let mut transition = self.candidate(base, state, now)?;
        let intent = if private_relay {
            None
        } else {
            if transition.checkpoint.code_intents.len() >= MAX_INTENTS {
                return Err(StoreError::TooLarge);
            }
            let mut id = [0; 32];
            getrandom::fill(&mut id).map_err(|_| StoreError::Storage)?;
            let id = CodeIntentId(id);
            transition.checkpoint.code_intents.push(StoredIntent {
                id,
                code: code.expose().into(),
                invitation_hash: hash,
                expires_ms,
                creator: self.identity.endpoint_id(),
                team: transition
                    .checkpoint
                    .document()
                    .ok_or(StoreError::Corrupt)?
                    .id
                    .clone(),
                founder: transition
                    .checkpoint
                    .document()
                    .ok_or(StoreError::Corrupt)?
                    .founder,
                incarnation: self.instance.incarnation,
                policy: codes.binding(),
                creation_revision: transition.checkpoint.revision,
                issued_ms: now,
                person,
            });
            transition
                .checkpoint
                .effects
                .push(ControlEffect::ReconcileCode {
                    id,
                    generation: transition.checkpoint.network_generation,
                });
            Some(id)
        };
        if transition.checkpoint.effects.len() > MAX_EFFECTS {
            return Err(StoreError::TooLarge);
        }
        validation::validate(&transition.checkpoint, &self.policy, now)?;
        checkpoint::encode(&transition.checkpoint)?;
        Ok(PendingCodeIntent {
            transition,
            code: if private_relay {
                ticket.clone()
            } else {
                code.expose().into()
            },
            ticket,
            intent,
            expires_ms,
        })
    }
    pub fn commit_code_intent(
        &mut self,
        pending: PendingCodeIntent,
        now: i64,
    ) -> Result<IssuedCodeInvite> {
        if now < 0 || now >= pending.expires_ms {
            return Err(StoreError::Invalid);
        }
        let outcome = self.commit_transition(pending.transition, now)?;
        match outcome {
            CommitOutcome::Active(loaded) => Ok(IssuedCodeInvite {
                loaded,
                code: pending.code,
                ticket: pending.ticket,
                intent: pending.intent,
            }),
            _ => Err(StoreError::Corrupt),
        }
    }
}
