use super::*;
impl TeamState {
    pub fn rotate_link(
        &self,
        actor: &SecretKey,
        now: i64,
        entropy: &mut impl Entropy,
    ) -> Result<Self> {
        self.actor(actor)?;
        let mut next = self.clone();
        if let Some(old) = self
            .document
            .link
            .as_ref()
            .and_then(|l| self.document.invites.get(&hash(&l.body.secret)))
        {
            let mut body = old.body.clone();
            body.expires = now;
            body.ts = next_ts(body.ts, now);
            next.sign_invite(actor, body)?;
        }
        let secret = random_secret(entropy)?;
        next.sign_invite(
            actor,
            InviteRecord {
                hash: hash(&secret),
                expires: i64::MAX / 4,
                person: None,
                used_by: None,
                ts: now,
                reusable: true,
            },
        )?;
        next.document.link = Some(
            SignedRecord::sign(
                LinkRecord {
                    secret,
                    ts: next_ts(self.document.link.as_ref().map_or(0, |l| l.body.ts), now),
                },
                actor,
                &self.document.id,
                &self.profile,
            )
            .map_err(|_| TeamError::Invalid)?,
        );
        Ok(next)
    }
    pub fn issue_invite(
        &self,
        actor: &SecretKey,
        secret: &str,
        hours: u64,
        own_machines: bool,
        now: i64,
    ) -> Result<Self> {
        self.actor(actor)?;
        let expires = now
            .checked_add(hours.clamp(1, 720) as i64 * 3_600_000)
            .ok_or(TeamError::ClockOverflow)?;
        let person = own_machines.then(|| {
            self.document.members[&actor.public().to_string()]
                .body
                .person
                .clone()
        });
        let mut next = self.clone();
        next.document
            .invites
            .retain(|_, invite| invite.body.expires > now.saturating_sub(7 * 86_400_000));
        next.sign_invite(
            actor,
            InviteRecord {
                hash: hash(secret),
                expires,
                person,
                used_by: None,
                ts: now,
                reusable: false,
            },
        )?;
        Ok(next)
    }
    pub fn admit(&self, actor: &SecretKey, request: Admission<'_>, now: i64) -> Result<Self> {
        self.actor(actor)?;
        let proof = request
            .proof
            .map(|token| self.verify_request(&request.endpoint, token, now))
            .transpose()?;
        let person = proof
            .as_ref()
            .map_or(request.person, |proof| proof.person());
        if !name(person) || !name(request.machine) {
            return Err(TeamError::Invalid);
        }
        let invitation = self
            .document
            .invites
            .get(&hash(request.secret))
            .ok_or(TeamError::UnknownInvite)?;
        if !self.is_active(invitation.by) {
            return Err(TeamError::InactiveInviter);
        }
        let body = &invitation.body;
        let retry = match body.used_by {
            Some(endpoint) if endpoint == request.endpoint && self.is_active(endpoint) => true,
            Some(_) => return Err(TeamError::InviteUsed),
            None => false,
        };
        if !retry {
            if body.expires <= now {
                return Err(if body.reusable {
                    TeamError::LinkReplaced
                } else {
                    TeamError::InviteExpired
                });
            }
            let same_account = proof
                .as_ref()
                .is_some_and(|proof| self.same_account(person, request.endpoint, proof, now));
            match &body.person {
                Some(expected) if expected != person => return Err(TeamError::WrongPerson),
                Some(person)
                    if &self.document.members[&actor.public().to_string()]
                        .body
                        .person
                        != person =>
                {
                    return Err(TeamError::OwnerRequired);
                }
                Some(_) => {}
                None if same_account
                    && request.owner_only
                    && self.document.members[&actor.public().to_string()]
                        .body
                        .person
                        != person =>
                {
                    return Err(TeamError::OwnerRequired);
                }
                None if same_account => {}
                None if self.document.members.values().any(|m| {
                    m.body.person == person
                        && m.body.endpoint != request.endpoint
                        && self.is_active(m.body.endpoint)
                }) =>
                {
                    return Err(TeamError::NameTaken);
                }
                None => {}
            }
        }
        // owner_only affects the proof-based same-account branch only; it does
        // not turn an otherwise unrestricted proof-free invite into owner-only.
        let mut next = self.clone();
        if !retry && !body.reusable {
            let mut used = body.clone();
            used.used_by = Some(request.endpoint);
            used.ts = next_ts(used.ts, now);
            next.sign_invite(actor, used)?;
        }
        next.put_member(actor, request.endpoint, person, request.machine, true, now)?;
        next.membership
            .apply_local_member(
                actor.public(),
                &next.document.members[&request.endpoint.to_string()],
                &next.profile,
                now,
            )
            .map_err(|_| TeamError::InvalidState)?;
        if let Some(token) = request.proof {
            next = next.attach_proof(request.endpoint, token, now)?;
        }
        Ok(next)
    }
    /// Targets must already be resolved by the future service selector. Removal
    /// signs all departures and rotates the link in one returned candidate.
    pub fn remove(
        &self,
        actor: &SecretKey,
        targets: &[EndpointId],
        now: i64,
        entropy: &mut impl Entropy,
    ) -> Result<Self> {
        self.actor(actor)?;
        if targets.is_empty()
            || targets.contains(&actor.public())
            || targets.iter().any(|id| !self.is_active(*id))
        {
            return Err(TeamError::Invalid);
        }
        let mut next = self.clone();
        for id in targets {
            let member = &self.document.members[&id.to_string()].body;
            next.put_member(actor, *id, &member.person, &member.machine, false, now)?;
        }
        next.refresh(actor.public(), now)?;
        next.rotate_link(actor, now, entropy)
    }
    /// Pure departure candidate. Online-peer policy, best-effort broadcast drain,
    /// local deletion and network shutdown are future service responsibilities.
    pub fn departure(&self, actor: &SecretKey, now: i64) -> Result<Self> {
        self.actor(actor)?;
        let member = &self.document.members[&actor.public().to_string()].body;
        let mut next = self.clone();
        next.put_member(
            actor,
            actor.public(),
            &member.person,
            &member.machine,
            false,
            now,
        )?;
        next.refresh(actor.public(), now)?;
        Ok(next)
    }
    fn sign_invite(&mut self, actor: &SecretKey, body: InviteRecord) -> Result<()> {
        let id = body.hash.clone();
        let signed = SignedRecord::sign(body, actor, &self.document.id, &self.profile)
            .map_err(|_| TeamError::Invalid)?;
        self.document.invites.insert(id, signed);
        Ok(())
    }
}
