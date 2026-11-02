use super::bootstrap_checks::{
    pins, reconcile_team, require_endpoint, validate_incoming_local, validate_local,
};
use super::*;
use crate::{
    daemon_config::ResolvedStartup,
    full_team_store::{FULL_TEAM_DATABASE, FullTeamStore},
    join_protocol::{JoinResult, join_peer},
    peer_session::{AuthorityStatus, PublicBootstrap, PublicCodeBootstrap, TextMailbox},
    team::JoinRequest,
};

pub(super) struct JoinOptions {
    pub founder: Option<EndpointId>,
    pub proof: Option<String>,
    pub own: bool,
    pub fence: Option<Arc<crate::peer_session::AdmissionFence>>,
}

pub(super) async fn open(
    root: &std::path::Path,
    engine: EngineAccess,
    settings: Arc<ResolvedNetwork>,
) -> Result<(Arc<AdmittedTeam>, Arc<crate::LocalIdentity>), ControlError> {
    let root = root.to_owned();
    tokio::task::spawn_blocking(move || {
        let directory = engine.directory().map_err(|_| ControlError::Storage)?;
        let lease = engine.lease().map_err(|_| ControlError::Storage)?;
        let identity = Arc::new(
            crate::load_or_create_identity(&directory, DEADLINE)
                .map_err(|_| ControlError::Storage)?,
        );
        let policy = settings.store_policy().map_err(|_| ControlError::Invalid)?;
        let store = match directory.symlink_metadata(FULL_TEAM_DATABASE) {
            Ok(_) => FullTeamStore::open_existing(&root, identity.clone(), policy, DEADLINE),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                FullTeamStore::create_new(&root, identity.clone(), policy, DEADLINE)
            }
            Err(_) => return Err(ControlError::Storage),
        }
        .map_err(|_| ControlError::Storage)?;
        let mail = crate::mail_store::MailStore::open(&root).map_err(|_| ControlError::Storage)?;
        let notification = engine.notification().map_err(|_| ControlError::Storage)?;
        let mailbox = TextMailbox::new(mail, move |address| engine.recipient(address))
            .with_notification(notification);
        let team = AdmittedTeam::open(store, lease, identity.endpoint_id(), now, Some(mailbox))
            .map_err(|_| ControlError::Storage)?;
        Ok((team, identity))
    })
    .await
    .map_err(|_| ControlError::Storage)?
}
pub(super) async fn startup(state: &State) -> Result<(), ControlError> {
    if let AuthorityStatus::Active { team, founder, .. } = state
        .authority
        .status()
        .await
        .map_err(|_| ControlError::Storage)?
    {
        pins(state, &team, founder)?;
        validate_local(state).await?;
        if let ResolvedStartup::Form {
            proof: Some(token), ..
        } = &state.settings.startup
        {
            let publication = state
                .authority
                .team_publication()
                .await
                .map_err(|_| ControlError::Rejected)?;
            let attached = publication
                .state()
                .attestations
                .contains_key(&state.authority.local().to_string());
            drop(publication);
            // Recover an interrupted form/proof sequence. Never silently resume
            // a proofless store when the configured proof failed previously.
            if !attached {
                state
                    .settings
                    .verify_ownership_proof(
                        &state.authority.local(),
                        token,
                        now().map_err(|_| ControlError::Storage)? / 1000,
                    )
                    .map_err(|_| ControlError::Rejected)?;
                drop(
                    state
                        .authority
                        .attach_proof(state.authority.local(), token.clone())
                        .await
                        .map_err(|_| ControlError::Rejected)?,
                );
            }
        }
        return Ok(());
    }
    match &state.settings.startup {
        ResolvedStartup::Existing => return Ok(()),
        ResolvedStartup::Form { name, relay, proof } => {
            // Formation generates a new root; an existing team pin cannot select it.
            if state.settings.expected_team.is_some() {
                return Err(ControlError::Rejected);
            }
            if let Some(token) = proof {
                state
                    .settings
                    .verify_ownership_proof(
                        &state.authority.local(),
                        token,
                        now().map_err(|_| ControlError::Storage)? / 1000,
                    )
                    .map_err(|_| ControlError::Rejected)?;
            }
            if let Some(relay) = relay {
                probe(state, relay).await?;
            }
            let publication = state
                .authority
                .form_with_relay(
                    name.clone(),
                    state.person.clone(),
                    state.machine.clone(),
                    relay.clone(),
                )
                .await
                .map_err(|_| ControlError::Rejected)?;
            drop(publication);
            if let Some(token) = proof {
                drop(
                    state
                        .authority
                        .attach_proof(state.authority.local(), token.clone())
                        .await
                        .map_err(|_| ControlError::Rejected)?,
                );
            }
        }
        ResolvedStartup::JoinTicket {
            ticket,
            expected_founder,
            proof,
            owner_only,
        } => {
            ticket_join(
                state,
                ticket,
                None,
                JoinOptions {
                    founder: *expected_founder,
                    proof: proof.clone(),
                    own: *owner_only,
                    fence: None,
                },
            )
            .await?;
        }
        ResolvedStartup::JoinCode {
            code,
            expected_team,
            expected_founder,
            proof,
            owner_only,
        } => {
            let policy = state
                .settings
                .code_policy()
                .map_err(|_| ControlError::Invalid)?
                .ok_or(ControlError::Invalid)?;
            let id = policy
                .derive(code)
                .map_err(|_| ControlError::Invalid)?
                .endpoint_key()
                .public();
            let address = state
                .settings
                .seeds
                .iter()
                .find(|a| a.id == id)
                .cloned()
                .unwrap_or_else(|| EndpointAddr::new(id));
            code_join(
                state,
                code,
                address,
                expected_team.clone(),
                JoinOptions {
                    founder: *expected_founder,
                    proof: proof.clone(),
                    own: *owner_only,
                    fence: None,
                },
            )
            .await?;
        }
    }
    if let AuthorityStatus::Active { team, founder, .. } = state
        .authority
        .status()
        .await
        .map_err(|_| ControlError::Storage)?
    {
        pins(state, &team, founder)?;
        validate_local(state).await?;
    }
    Ok(())
}
pub(super) async fn ticket_join(
    state: &State,
    ticket: &crate::ticket::Ticket,
    address: Option<EndpointAddr>,
    options: JoinOptions,
) -> Result<(), ControlError> {
    reconcile_team(state.settings.expected_team.as_deref(), Some(&ticket.team))?;
    let mut candidates = ticket.peer_addresses();
    for candidate in &mut candidates {
        if let Some(seed) = state.settings.seeds.iter().find(|a| a.id == candidate.id) {
            candidate.addrs.extend(seed.addrs.iter().cloned());
        }
    }
    if let Some(address) = address {
        if !ticket.peers.contains(&address.id) {
            return Err(ControlError::Rejected);
        }
        candidates.insert(0, address);
    }
    let request = JoinRequest {
        team: ticket.team.clone(),
        secret: ticket.secret.clone(),
        person: state.person.clone(),
        machine: state.machine.clone(),
        proof: options.proof.clone(),
        owner_only: options.own,
    };
    for address in candidates {
        if let Ok(JoinResult::Joined(joined)) =
            join_peer(&state.endpoint, address.clone(), &request, DEADLINE).await
        {
            validate_incoming_local(state, &joined.state)?;
            let input = PublicBootstrap {
                expected_team: ticket.team.clone(),
                expected_founder: options.founder,
                authenticated_inviter: joined.peer,
                state: joined.state,
            };
            match &options.fence {
                Some(fence) => {
                    state
                        .authority
                        .join_existing_fenced(input, fence.clone())
                        .await
                }
                None => state.authority.join_existing(input).await,
            }
            .map_err(|_| ControlError::Rejected)?;
            state.addresses.write().await.insert(address.id, address);
            return Ok(());
        }
    }
    Err(ControlError::Transport)
}
pub(super) async fn code_join(
    state: &State,
    code: &crate::code_invite::CanonicalCode,
    address: EndpointAddr,
    team: Option<String>,
    options: JoinOptions,
) -> Result<(), ControlError> {
    let team = reconcile_team(state.settings.expected_team.as_deref(), team.as_deref())?;
    let policy = state
        .settings
        .code_policy()
        .map_err(|_| ControlError::Invalid)?
        .ok_or(ControlError::Invalid)?;
    let derived = policy.derive(code).map_err(|_| ControlError::Invalid)?;
    // Reject an unrelated transport identity before constructing or sending the bearer.
    require_endpoint(derived.endpoint_key().public(), address.id)?;
    let request = JoinRequest {
        team: team.clone().unwrap_or_default(),
        secret: derived.bearer_secret().into(),
        person: state.person.clone(),
        machine: state.machine.clone(),
        proof: options.proof.clone(),
        owner_only: options.own,
    };
    let joined = match join_peer(&state.endpoint, address, &request, DEADLINE)
        .await
        .map_err(|_| ControlError::Transport)?
    {
        JoinResult::Joined(joined) => joined,
        JoinResult::Refused(_) => return Err(ControlError::Rejected),
    };
    if team
        .as_deref()
        .is_some_and(|expected| expected != joined.state.id)
    {
        return Err(ControlError::Rejected);
    }
    let anchor = policy
        .authenticate_endpoint(code, joined.peer)
        .map_err(|_| ControlError::Rejected)?;
    validate_incoming_local(state, &joined.state)?;
    let input = PublicCodeBootstrap {
        anchor,
        expected_team: team,
        expected_founder: options.founder,
        state: joined.state,
    };
    match options.fence {
        Some(fence) => {
            state
                .authority
                .join_code_existing_fenced(input, fence)
                .await
        }
        None => state.authority.join_code_existing(input).await,
    }
    .map_err(|_| ControlError::Rejected)
}

pub(super) async fn probe(state: &State, relay: &str) -> Result<(), ControlError> {
    let mut config = state.settings.endpoint_config(iroh::SecretKey::generate());
    config.relay =
        crate::RelaySelection::Custom(vec![relay.parse().map_err(|_| ControlError::Invalid)?]);
    config.direct_paths = false;
    config.bind_address.set_port(0);
    let endpoint = NetworkEndpoint::bind(config)
        .await
        .map_err(|_| ControlError::Transport)?;
    let result = endpoint
        .online(DEADLINE)
        .await
        .map_err(|_| ControlError::Transport);
    endpoint.shutdown().await;
    result
}
