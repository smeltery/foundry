//! Internal full-team hooks; public projection merge remains proof-free and strict.
use super::*;
impl MembershipState {
    pub(crate) fn restore_verified(
        snapshot: &crate::full_team_store::VerifiedLocalSnapshot<'_>,
    ) -> Self {
        let document = snapshot.document();
        Self {
            signing_prefix: snapshot.profile().signing_prefix.clone(),
            snapshot: MembershipSnapshot {
                team: document.id.clone(),
                founder: document.founder,
                metadata: document.meta.clone(),
                admissions: document.admissions.clone(),
                members: document.members.clone(),
            },
        }
    }

    pub(super) fn proven_owner(
        &self,
        body: &MemberRecord,
        owners: &BTreeMap<EndpointId, (String, String)>,
    ) -> bool {
        let Some((subject, person)) = owners.get(&body.endpoint) else {
            return false;
        };
        person == &body.person
            && self.snapshot.members.values().any(|member| {
                let other = &member.body;
                other.endpoint != body.endpoint
                    && other.person == body.person
                    && self.is_active(other.endpoint)
                    && owners
                        .get(&other.endpoint)
                        .is_some_and(|owner| &owner.0 == subject && &owner.1 == person)
            })
    }
    /// Only the full-team local admission path calls this after invitation policy.
    /// This does not change incoming merge's name or self-return checks.
    pub(crate) fn apply_local_member(
        &mut self,
        actor: EndpointId,
        record: &SignedRecord<MemberRecord>,
        profile: &WireProfile,
        now: i64,
    ) -> Result<()> {
        let body = &record.body;
        if !self.is_active(actor)
            || record.by != actor
            || !body.active
            || profile.signing_prefix != self.signing_prefix
            || !valid_names(body)
            || !plausible(body.ts, now)
        {
            return Err(NetworkError::Invalid("invalid local member transition"));
        }
        profile.validate()?;
        record.verify(&self.snapshot.team, profile)?;
        let id = body.endpoint.to_string();
        self.snapshot
            .admissions
            .entry(id.clone())
            .or_insert_with(|| record.clone());
        self.snapshot.members.insert(id, record.clone());
        Ok(())
    }
}
