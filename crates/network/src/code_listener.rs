//! Live code-invitation endpoint. Discovery/publication is explicitly host-owned.
use crate::{
    EndpointConfig, NetworkEndpoint, RelaySelection, WireProfile,
    full_team_store::CodeLease,
    join_protocol::{Refusal, serve_join},
    peer_session::{AdmissionFence, AdmittedTeam, CodePermit, PeerError},
};
use iroh::EndpointAddr;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{sync::watch, task::JoinSet};

pub struct CodeTransport {
    pub relay: RelaySelection,
    pub bind_address: SocketAddr,
    pub direct_paths: bool,
    pub relay_root_certificates: Vec<Vec<u8>>,
    pub timeout: Duration,
}

/// Keep this permit through the bounded external publication operation. The
/// address contains a code-derived identity and is privileged, not a status row.
pub struct CodePublication {
    _permit: CodePermit,
    address: EndpointAddr,
}
impl CodePublication {
    pub fn address(&self) -> &EndpointAddr {
        &self.address
    }
}

pub struct CodeListener {
    endpoint: NetworkEndpoint,
    authority: Arc<AdmittedTeam>,
    lease: Arc<CodeLease>,
    profile: WireProfile,
    timeout: Duration,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeExit {
    Joined,
    Stopped,
    RefreshRequired,
}

impl CodeListener {
    /// Validate before bind and again after it. The actual store policy supplies
    /// the protocol profile; transport configuration cannot replace that policy.
    pub async fn bind(
        authority: Arc<AdmittedTeam>,
        lease: Arc<CodeLease>,
        transport: CodeTransport,
    ) -> Result<Self, PeerError> {
        if transport.timeout.is_zero() || transport.timeout > Duration::from_secs(60) {
            return Err(PeerError::Invalid);
        }
        let permit = authority.code_permit(lease.clone()).await?;
        let profile = permit.profile().clone();
        let config = EndpointConfig {
            profile: profile.clone(),
            secret_key: permit.derived().endpoint_key().clone(),
            relay: transport.relay,
            bind_address: transport.bind_address,
            direct_paths: transport.direct_paths,
            relay_root_certificates: transport.relay_root_certificates,
        };
        let endpoint = tokio::time::timeout(transport.timeout, NetworkEndpoint::bind(config))
            .await
            .map_err(|_| PeerError::TimedOut)?
            .map_err(|_| PeerError::Transport)?;
        drop(permit);
        let checked = authority.code_permit(lease.clone()).await;
        match checked {
            Ok(permit) if permit.derived().endpoint_key().public() == endpoint.id() => {
                drop(permit);
                Ok(Self {
                    endpoint,
                    authority,
                    lease,
                    profile,
                    timeout: transport.timeout,
                })
            }
            other => {
                let error = other.err().unwrap_or(PeerError::Unauthorized);
                endpoint.shutdown().await;
                Err(error)
            }
        }
    }

    /// A fresh writer permit prevents revocation between validation and the
    /// host's publication. Dropping this value ends that publication authority.
    pub async fn publication(&self) -> Result<CodePublication, PeerError> {
        let permit = self.authority.code_permit(self.lease.clone()).await?;
        if permit.derived().endpoint_key().public() != self.endpoint.id() {
            return Err(PeerError::Unauthorized);
        }
        Ok(CodePublication {
            _permit: permit,
            address: self.endpoint.address(),
        })
    }

    /// Stop accepting when the intent changes or is consumed. Existing replies
    /// finish under their own admission guards, then drain before endpoint close.
    /// Malformed/refused clients do not consume an otherwise valid invitation.
    /// The one-second accept window bounds only waiting for a client; each
    /// handshake runs in the task set under the listener timeout and counts
    /// toward its capacity.
    pub async fn serve(self, mut cancel: watch::Receiver<bool>) -> Result<CodeExit, PeerError> {
        let mut tasks = JoinSet::new();
        // Set when serving ends, however it ends, so in-flight work stops too.
        let (halt, halted) = watch::channel(false);
        let mut outcome = CodeExit::Stopped;
        let mut failure = None;
        loop {
            if *cancel.borrow() || cancel.has_changed().is_err() {
                break;
            }
            match self.authority.code_permit(self.lease.clone()).await {
                Ok(permit) => drop(permit),
                Err(PeerError::StaleEpoch | PeerError::Unauthorized) => {
                    outcome = CodeExit::RefreshRequired;
                    break;
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
            if *cancel.borrow() || cancel.has_changed().is_err() {
                break;
            }
            tokio::select! {
                _ = cancel.changed() => break,
                joined = tasks.join_next(), if !tasks.is_empty() => {
                    match joined {
                        Some(Ok(result)) => {
                            if matches!(result,Ok(true)) { outcome=CodeExit::Joined; break; }
                        }
                        Some(Err(_)) => { failure=Some(PeerError::Storage); break; }
                        _ => {}
                    }
                }
                incoming = self.endpoint.incoming(Duration::from_secs(1)) => {
                    match incoming {
                        Ok(handshake) if tasks.len() < 8 => {
                            let fence=Arc::new(AdmissionFence::new(cancel.clone()));
                            let authority=self.authority.clone();
                            let lease=self.lease.clone();
                            let profile=self.profile.clone();
                            let timeout=self.timeout;
                            let halted=halted.clone();
                            tasks.spawn(async move {
                                let connection=tokio::select! {
                                    biased;
                                    _ = until_halted(halted.clone()) => return Ok(false),
                                    connection = handshake.complete(timeout) => connection?,
                                };
                                let admission=fence.clone();
                                let serving=serve_join(connection.clone(),&profile,timeout, move |peer,request| async move {
                                    authority.admit_code_join_fenced(peer,request,lease,admission).await.map_err(|error|match error {
                                        PeerError::Storage|PeerError::Quiescent => Refusal::Unavailable,
                                        _ => Refusal::Denied,
                                    })
                                });
                                tokio::pin!(serving);
                                tokio::select! {
                                    biased;
                                    result = &mut serving => result,
                                    _ = until_halted(halted) => {
                                        // Pending readers cannot become fresh admissions after stop. A
                                        // commit that won the fence already started and keeps its reply.
                                        if fence.stop_pending() {
                                            connection.close(0u32.into(), b"invitation stopped");
                                        }
                                        serving.await
                                    }
                                }
                            });
                        }
                        Ok(handshake) => handshake.refuse(),
                        Err(crate::NetworkError::TimedOut) => {}
                        Err(_) => { failure=Some(PeerError::Transport); break; }
                    }
                }
            }
        }
        halt.send_replace(true);
        while let Some(joined) = tasks.join_next().await {
            if matches!(joined, Ok(Ok(true))) {
                outcome = CodeExit::Joined;
            }
            if joined.is_err() {
                failure = Some(PeerError::Storage);
            }
        }
        self.endpoint.shutdown().await;
        match failure {
            Some(error) => Err(error),
            None => Ok(outcome),
        }
    }

    pub async fn shutdown(self) {
        self.endpoint.shutdown().await;
    }
}

async fn until_halted(mut halted: watch::Receiver<bool>) {
    // A dropped sender also means serving has ended.
    let _ = halted.wait_for(|halted| *halted).await;
}
