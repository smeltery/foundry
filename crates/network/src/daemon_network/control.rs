use super::{
    protocol::{Command, Status},
    *,
};
use crate::{
    peer_session::{IssuedInvitation, TeamPublication},
    ticket::Ticket,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::AsyncWrite;

// Privileged output owns its authority permit until the bounded IPC write ends.
pub(crate) struct NetworkResponse {
    value: Value,
    _guard: Option<PublicationGuard>,
    after: Option<(
        Arc<AdmittedTeam>,
        Vec<crate::full_team_store::ControlEffect>,
    )>,
}
pub(super) enum PublicationGuard {
    Team {
        _publication: TeamPublication,
    },
    Code {
        _invitation: IssuedInvitation,
    },
    Permit {
        _permit: crate::peer_session::CodePermit,
    },
}
impl NetworkResponse {
    pub(crate) fn error(id: u64, error: ControlError) -> Self {
        Self {
            value: json!({"id":id,"result":{"status":"error","value":error}}),
            _guard: None,
            after: None,
        }
    }
    pub(super) fn success(
        id: u64,
        value: impl Serialize,
        guard: Option<PublicationGuard>,
    ) -> Result<Self, ControlError> {
        let value = serde_json::to_value(value).map_err(|_| ControlError::Invalid)?;
        Ok(Self {
            value: json!({"id":id,"result":{"status":"ok","value":value}}),
            _guard: guard,
            after: None,
        })
    }
    pub(super) fn acknowledge_after_write(
        mut self,
        authority: Arc<AdmittedTeam>,
        effects: Vec<crate::full_team_store::ControlEffect>,
    ) -> Self {
        self.after = Some((authority, effects));
        self
    }
    pub(crate) async fn write(self, writer: &mut (impl AsyncWrite + Unpin)) -> crate::Result<()> {
        // Keep all guard fields live until write_record has flushed or failed.
        let result =
            crate::ipc_codec::write_record(writer, &self.value, Duration::from_secs(5)).await;
        let Self { _guard, after, .. } = self;
        drop(_guard);
        result?;
        if let Some((authority, effects)) = after {
            for effect in effects {
                match authority.acknowledge_code_effect(effect).await {
                    Ok(()) | Err(crate::peer_session::PeerError::StaleEpoch) => {}
                    Err(_) => {
                        return Err(crate::NetworkError::Invalid(
                            "network publication acknowledgement failed",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}
impl State {
    pub(super) async fn control(
        &self,
        id: u64,
        command: Command,
        fence: Arc<crate::peer_session::AdmissionFence>,
    ) -> Result<NetworkResponse, ControlError> {
        let changed = json!({"kind":"changed"});
        let response = match command {
            Command::Status {} => {
                let (team, revision, peers) =
                    match self.authority.status().await.map_err(rejected)? {
                        crate::peer_session::AuthorityStatus::Unjoined => (None, None, Vec::new()),
                        crate::peer_session::AuthorityStatus::Active { team, revision, .. } => {
                            let view = self.authority.routing_view().await.map_err(rejected)?;
                            (
                                Some(team),
                                Some(revision),
                                view.members
                                    .iter()
                                    .filter(|m| m.active && m.endpoint != view.local)
                                    .map(|m| m.endpoint)
                                    .collect(),
                            )
                        }
                    };
                NetworkResponse::success(
                    id,
                    json!({"kind":"network_status","status":Status {
                        endpoint:self.endpoint.address(),team,revision,peers,
                    }}),
                    None,
                )?
            }
            Command::Form { name } => {
                if self.settings.expected_team.is_some() {
                    return Err(ControlError::Rejected);
                }
                let guard = self
                    .authority
                    .form(name, self.person.clone(), self.machine.clone())
                    .await
                    .map_err(rejected)?;
                NetworkResponse::success(
                    id,
                    changed,
                    Some(PublicationGuard::Team {
                        _publication: guard,
                    }),
                )?
            }
            Command::Invite { hours, own, short } => {
                if short {
                    let issued = self
                        .authority
                        .issue_code(hours, own)
                        .await
                        .map_err(rejected)?;
                    NetworkResponse::success(
                        id,
                        json!({"kind":"invitation","ticket":issued.issued().ticket(),
                        "code":issued.issued().code(),"endpoint":self.endpoint.address(),
                        "intent":issued.issued().listener_intent()}),
                        Some(PublicationGuard::Code {
                            _invitation: issued,
                        }),
                    )?
                } else {
                    let secret = data_encoding::HEXLOWER
                        .encode(&iroh::SecretKey::generate().to_bytes()[..16]);
                    let guard = self
                        .authority
                        .issue_invite(secret.clone(), hours, own)
                        .await
                        .map_err(rejected)?;
                    let document = guard.state();
                    let address = self.endpoint.address();
                    let ticket = Ticket {
                        team: document.id.clone(),
                        secret,
                        name: document.meta.body.name.clone(),
                        relay: document.meta.body.relay.clone(),
                        hint_relay: address.relay_urls().next().map(ToString::to_string),
                        hint_ips: address.ip_addrs().copied().collect(),
                        peers: vec![address.id],
                    };
                    let text = self
                        .settings
                        .ticket_codec()
                        .map_err(|_| ControlError::Invalid)?
                        .encode(&ticket)
                        .map_err(|_| ControlError::Invalid)?;
                    NetworkResponse::success(
                        id,
                        json!({"kind":"invitation","ticket":text,"endpoint":address}),
                        Some(PublicationGuard::Team {
                            _publication: guard,
                        }),
                    )?
                }
            }
            Command::Join {
                ticket,
                address,
                founder,
            } => {
                let ticket = self
                    .settings
                    .ticket_codec()
                    .map_err(|_| ControlError::Invalid)?
                    .decode(&ticket)
                    .map_err(|_| ControlError::Invalid)?;
                bootstrap::ticket_join(
                    self,
                    &ticket,
                    address,
                    bootstrap::JoinOptions {
                        founder,
                        proof: None,
                        own: false,
                        fence: Some(fence),
                    },
                )
                .await?;
                NetworkResponse::success(id, changed, None)?
            }
            Command::JoinCode {
                code,
                address,
                team,
                founder,
            } => {
                let policy = self
                    .settings
                    .code_policy()
                    .map_err(|_| ControlError::Invalid)?
                    .ok_or(ControlError::Invalid)?;
                let code = policy.normalize(&code).map_err(|_| ControlError::Invalid)?;
                bootstrap::code_join(
                    self,
                    &code,
                    address,
                    team,
                    bootstrap::JoinOptions {
                        founder,
                        proof: None,
                        own: false,
                        fence: Some(fence),
                    },
                )
                .await?;
                NetworkResponse::success(id, changed, None)?
            }
            Command::Connect { address } => {
                let (reply, receive) = tokio::sync::oneshot::channel();
                self.connect
                    .send(supervisor::Connect { address, reply })
                    .await
                    .map_err(|_| ControlError::Transport)?;
                receive.await.map_err(|_| ControlError::Transport)??;
                NetworkResponse::success(id, changed, None)?
            }
            Command::Remove { endpoints } => {
                self.authority.remove(endpoints).await.map_err(rejected)?;
                NetworkResponse::success(id, changed, None)?
            }
            Command::Rotate {} => {
                let guard = self.authority.rotate_link().await.map_err(rejected)?;
                NetworkResponse::success(
                    id,
                    changed,
                    Some(PublicationGuard::Team {
                        _publication: guard,
                    }),
                )?
            }
            Command::Depart {} => {
                self.authority.depart().await.map_err(rejected)?;
                self.engine
                    .with(|e| {
                        e.suspend_remote_directory();
                        Ok(())
                    })
                    .await
                    .map_err(|_| ControlError::Storage)?;
                NetworkResponse::success(id, changed, None)?
            }
            Command::CodePublications {} => return self.codes.publication(self, id).await,
        };
        self.wake.notify_one();
        Ok(response)
    }
}
fn rejected(error: crate::peer_session::PeerError) -> ControlError {
    use crate::peer_session::PeerError;
    match error {
        PeerError::Storage | PeerError::Quiescent => ControlError::Storage,
        _ => ControlError::Rejected,
    }
}
