use super::*;
impl TeamState {
    /// Return a candidate and report; publish it only after the future full-team
    /// store commits. Current state remains untouched on every failure.
    pub fn merge_from(
        &self,
        authenticated_peer: EndpointId,
        incoming: &TeamDocument,
        now_ms: i64,
    ) -> Result<(Self, MergeReport)> {
        self.merge_inner(authenticated_peer, incoming, now_ms, None)
    }
    pub(crate) fn merge_for_local(
        &self,
        peer: EndpointId,
        incoming: &TeamDocument,
        now: i64,
        local: EndpointId,
    ) -> Result<(Self, MergeReport)> {
        self.merge_inner(peer, incoming, now, Some(local))
    }
    fn merge_inner(
        &self,
        authenticated_peer: EndpointId,
        incoming: &TeamDocument,
        now_ms: i64,
        local: Option<EndpointId>,
    ) -> Result<(Self, MergeReport)> {
        if self.verifier.is_none() {
            no_proofs(&self.document)?;
            no_proofs(incoming)?;
        }
        if incoming.id != self.document.id || incoming.founder != self.document.founder {
            return Err(TeamError::DifferentTeam);
        }
        if !self.is_active(authenticated_peer) {
            return Err(TeamError::Unauthorized);
        }
        let mut next = self.clone();
        let mut proof_report = MergeReport::default();
        proofs::merge_attestations(
            &mut next.document,
            incoming,
            self.verifier.as_deref(),
            now_ms,
            &mut proof_report,
        )?;
        let owners = proofs::ownership(&next.document, self.verifier.as_deref(), now_ms);
        let mut report = next
            .membership
            .merge_with_ownership(
                authenticated_peer,
                &projection(incoming),
                &self.profile,
                now_ms,
                &owners,
            )
            .map_err(|_| TeamError::InvalidState)?;
        next.sync_membership();
        next.prune_proofs();
        report.changed |= next.document.attestations != self.document.attestations;
        report.rejected += proof_report.rejected;
        if local.is_some_and(|id| !next.is_active(id)) {
            next.document.invites.clear();
            next.document.link = None;
            next.document.attestations.clear();
        } else {
            next.merge_secrets(incoming, now_ms, &mut report);
        }
        Ok((next, report))
    }
    pub(super) fn merge_secrets(
        &mut self,
        incoming: &TeamDocument,
        now: i64,
        report: &mut MergeReport,
    ) {
        if let Some(link) = &incoming.link
            && self
                .document
                .link
                .as_ref()
                .is_none_or(|old| (link.body.ts, &link.sig) > (old.body.ts, &old.sig))
        {
            if link.body.ts <= now.saturating_add(86_400_000)
                && self.trusted(link.by)
                && link.verify(&self.document.id, &self.profile).is_ok()
            {
                self.document.link = Some(link.clone());
                report.changed = true;
            } else {
                report.rejected += 1;
            }
        }
        for (id, invite) in &incoming.invites {
            if self
                .document
                .invites
                .get(id)
                .is_some_and(|old| (invite.body.ts, &invite.sig) <= (old.body.ts, &old.sig))
            {
                continue;
            }
            if id == &invite.body.hash
                && invite.body.ts <= now.saturating_add(86_400_000)
                && self.trusted(invite.by)
                && invite.verify(&self.document.id, &self.profile).is_ok()
            {
                self.document.invites.insert(id.clone(), invite.clone());
                report.changed = true;
            } else {
                report.rejected += 1;
            }
        }
    }
}
