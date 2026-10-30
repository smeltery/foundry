use super::*;

/// Descriptive roster data only. It cannot grant a writer/admission permit.
pub struct RoutingMember {
    pub endpoint: EndpointId,
    pub person: String,
    pub machine: String,
    pub active: bool,
}
pub struct RoutingView {
    pub team: String,
    pub founder: EndpointId,
    pub local: EndpointId,
    pub revision: u64,
    pub name: String,
    pub relay: Option<String>,
    pub members: Vec<RoutingMember>,
    // Keep current authority stable through the caller's synchronous Engine
    // update. Drop the view before awaiting a peer writer or another gate call.
    _guard: OwnedMutexGuard<Gate>,
}
impl AdmittedTeam {
    /// Reload under the same actual ownership gate used by every peer effect.
    /// FullTeamStore bounds and verifies the document before projection.
    pub async fn routing_view(&self) -> Result<RoutingView> {
        let mut guard = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let (document, revision) = guard.current()?;
            let members = document
                .members
                .values()
                .map(|record| {
                    let body = &record.body;
                    RoutingMember {
                        endpoint: body.endpoint,
                        person: body.person.clone(),
                        machine: body.machine.clone(),
                        active: body.active
                            && document.admissions.contains_key(&body.endpoint.to_string()),
                    }
                })
                .collect();
            Ok(RoutingView {
                team: document.id,
                founder: document.founder,
                local: guard.local,
                revision,
                name: document.meta.body.name,
                relay: document.meta.body.relay,
                members,
                _guard: guard,
            })
        })
        .await
        .map_err(|_| PeerError::Storage)?
    }
}
