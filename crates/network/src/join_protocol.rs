//! Bounded JOIN exchange. The admission owner commits before returning success;
//! clients still verify the returned team against independently selected trust.
use crate::{
    Channel, NetworkEndpoint, NetworkError, Result, WireProfile,
    codec::{read_json_frame, write_json_frame},
    team::{JoinReply, JoinRequest, TeamDocument},
};
use iroh::{EndpointAddr, EndpointId, endpoint::Connection};
use serde::Serialize;
use std::{future::Future, time::Duration};

/// Secret-bearing state deliberately has no diagnostic representation.
pub struct Joined {
    pub peer: EndpointId,
    pub state: TeamDocument,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Invalid,
    Denied,
    Unavailable,
}
impl Refusal {
    fn message(self) -> &'static str {
        match self {
            Self::Invalid => "invalid invitation request",
            Self::Denied => "invitation refused",
            Self::Unavailable => "invitation state unavailable",
        }
    }
}
pub enum JoinResult {
    Joined(Box<Joined>),
    Refused(String),
}
struct Close(Connection);
impl Drop for Close {
    fn drop(&mut self) {
        self.0.close(0u32.into(), b"join exchange finished");
    }
}

/// The deadline covers connect, stream setup, request and reply together. There
/// is no implicit discovery, self-admission, retry or reply verification here.
pub async fn join_peer(
    endpoint: &NetworkEndpoint,
    address: EndpointAddr,
    request: &JoinRequest,
    timeout: Duration,
) -> Result<JoinResult> {
    if timeout.is_zero() {
        return Err(NetworkError::Invalid("positive join timeout required"));
    }
    tokio::time::timeout(timeout, async {
        let connection = endpoint.connect(address, Channel::Join, timeout).await?;
        let close = Close(connection);
        let (mut send, mut receive) = close.0.open_bi().await.map_err(transport)?;
        write_json_frame(&mut send, request, timeout).await?;
        send.finish().map_err(transport)?;
        let reply = read_json_frame::<JoinReply>(&mut receive, timeout)
            .await?
            .ok_or(NetworkError::Closed)?;
        Ok(match reply {
            JoinReply::Ok { state } => JoinResult::Joined(Box::new(Joined {
                peer: close.0.remote_id(),
                state: *state,
            })),
            JoinReply::Err { error } => JoinResult::Refused(error.replace(char::is_control, " ")),
        })
    })
    .await
    .map_err(|_| NetworkError::TimedOut)?
}

/// The callback receives the transport-authenticated joiner, never an asserted
/// identity from JSON. It must verify and durably commit before returning state.
/// Retain its actual ownership guard through any detached blocking work.
///
/// Cancellation closes the connection. After a successful reply, keep the
/// existing connection alive for at most five seconds so a consumed invitation
/// can finish its reply; this does not authorize a fresh listener or new join.
pub async fn serve_join<F, Fut, Approval>(
    connection: Connection,
    profile: &WireProfile,
    timeout: Duration,
    admit: F,
) -> Result<bool>
where
    F: FnOnce(EndpointId, JoinRequest) -> Fut,
    Fut: Future<Output = std::result::Result<Approval, Refusal>>,
    Approval: AsRef<TeamDocument>,
{
    let close = Close(connection);
    if timeout.is_zero() {
        return Err(NetworkError::Invalid("positive join timeout required"));
    }
    profile.validate()?;
    if close.0.alpn() != profile.join_alpn {
        return Err(NetworkError::Invalid("join channel required"));
    }
    let accepted = tokio::time::timeout(timeout, async {
        let (mut send, mut receive) = close.0.accept_bi().await.map_err(transport)?;
        let request = read_json_frame::<JoinRequest>(&mut receive, timeout)
            .await?
            .ok_or(NetworkError::Closed)?;
        let approval = admit(close.0.remote_id(), request).await;
        let accepted = approval.is_ok();
        #[derive(Serialize)]
        #[serde(tag = "t", rename_all = "snake_case")]
        enum Reply<'a> {
            Ok { state: &'a TeamDocument },
            Err { error: &'static str },
        }
        let reply = match &approval {
            Ok(state) => Reply::Ok {
                state: state.as_ref(),
            },
            Err(refusal) => Reply::Err {
                error: refusal.message(),
            },
        };
        write_json_frame(&mut send, &reply, timeout).await?;
        send.finish().map_err(transport)?;
        // A guarded approval holds the membership writer permit through all
        // output, then releases it before the bounded old-connection drain.
        drop(approval);
        Ok::<_, NetworkError>(accepted)
    })
    .await
    .map_err(|_| NetworkError::TimedOut)??;
    let _ = tokio::time::timeout(Duration::from_secs(5), close.0.closed()).await;
    Ok(accepted)
}
fn transport(error: impl std::fmt::Display) -> NetworkError {
    NetworkError::Transport(error.to_string())
}
