//! Synchronous local dispatcher. Host owns single-instance lease, private IPC,
//! fresh connection IDs and blocking-work scheduling. No external adapter effects.
use crate::{mail_store as mail, session_registry as registry};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
#[path = "daemon_engine/convert.rs"]
mod convert;
#[path = "daemon_engine/delivery.rs"]
mod delivery;
#[path = "daemon_engine/inbox.rs"]
mod inbox;
#[path = "daemon_engine/operations.rs"]
mod operations;
#[path = "daemon_engine/remote.rs"]
mod remote;
#[path = "daemon_engine/routing.rs"]
mod routing;
pub use mail::RemoteMember;
#[path = "daemon_engine/protocol.rs"]
mod protocol;
#[path = "daemon_engine/waiting.rs"]
mod waiting;
pub use protocol::*;
pub use registry::ConnectionId;
use std::sync::Arc;
use tokio::sync::Notify;
pub use waiting::InboxWait;
use waiting::WaitState;

pub struct TrustedIdentity {
    pub person: String,
    pub machine: String,
}
struct Connection {
    scope: String,
}
pub struct Engine {
    registry: registry::DurableRegistry,
    mail: mail::MailStore,
    identity: TrustedIdentity,
    connections: BTreeMap<ConnectionId, Connection>,
    clock: Box<dyn Fn() -> Result<i64, EngineError> + Send>,
    uncertain: bool,
    remote_ready: bool,
    remote_generations: BTreeMap<iroh::EndpointId, (u64, bool)>,
    changed: Arc<Notify>,
    waits: BTreeMap<String, Arc<WaitState>>,
}
const MAX_CONNECTIONS: usize = mail::MAX_POLL_SCOPES;
const MAX_ACTIVE_WAITS: usize = 128;
pub const MAX_WAIT_SECS: u64 = 600;
impl Engine {
    /// The caller holds the state-root daemon lease for the engine's lifetime.
    pub fn open(
        root: &Path,
        policy: registry::FolderPolicy,
        identity: TrustedIdentity,
    ) -> Result<Self, EngineError> {
        Self::open_with_clock(root, policy, identity, || {
            let ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| EngineError::Clock)?
                .as_millis();
            i64::try_from(ms).map_err(|_| EngineError::Clock)
        })
    }
    /// Host-only clock injection. Client JSON never supplies time. Values are
    /// nonnegative epoch milliseconds; rollback may delay durable retry recovery.
    pub fn open_with_clock(
        root: &Path,
        policy: registry::FolderPolicy,
        identity: TrustedIdentity,
        clock: impl Fn() -> Result<i64, EngineError> + Send + 'static,
    ) -> Result<Self, EngineError> {
        for value in [&identity.person, &identity.machine] {
            if value.is_empty()
                || value.len() > registry::MAX_TEXT_BYTES
                || value.chars().any(char::is_control)
            {
                return Err(EngineError::InvalidInput);
            }
        }
        let registry = registry::DurableRegistry::open(root, policy, Duration::from_secs(5))?;
        let mut mail = mail::MailStore::open(root)?;
        mail.recover_polling()?;
        mail.recover_transport()?;
        Ok(Self {
            registry,
            mail,
            identity,
            connections: BTreeMap::new(),
            clock: Box::new(clock),
            uncertain: false,
            remote_ready: false,
            remote_generations: BTreeMap::new(),
            changed: Arc::new(Notify::new()),
            waits: BTreeMap::new(),
        })
    }
    pub fn handle(&mut self, connection: ConnectionId, envelope: RequestEnvelope) -> Reply {
        let result = self.dispatch(connection, envelope.request);
        if matches!(result, Err(EngineError::StorageUncertain)) {
            self.uncertain = true;
        }
        if matches!(
            result,
            Ok(Outcome::Sent { .. })
                | Ok(Outcome::TransportQueued { .. })
                | Ok(Outcome::PushFailed)
        ) {
            self.changed.notify_waiters();
        }
        Reply {
            id: envelope.id,
            result: match result {
                Ok(value) => ReplyResult::Ok(value),
                Err(error) => ReplyResult::Error(error),
            },
        }
    }
    fn dispatch(
        &mut self,
        connection: ConnectionId,
        request: Request,
    ) -> Result<Outcome, EngineError> {
        if self.uncertain {
            return Err(EngineError::StorageUncertain);
        }
        self.ensure_connection(connection)?;
        match request {
            Request::Register {
                name,
                repository,
                share_path,
            } => self.register(connection, name, repository, share_path),
            Request::Attach { auth } => self.attach(connection, auth),
            Request::Who { filter } => self.who(filter),
            Request::SetStatus {
                auth,
                status,
                doing,
            } => {
                self.authorize(connection, &auth)?;
                self.registry.transact(|r| {
                    r.set_status(
                        &auth.conversation.native(),
                        &auth.secret(),
                        connection,
                        &status,
                        &doing,
                    )
                })?;
                Ok(Outcome::StatusUpdated)
            }
            Request::Linked {} => Ok(Outcome::Linked {
                conversation: self
                    .registry
                    .registry()?
                    .linked(connection)?
                    .conversation
                    .into(),
            }),
            Request::Send {
                auth,
                message_id,
                to,
                body,
                reply_to,
                wake,
                notify,
            } => self.send(
                connection,
                auth,
                mail::Message {
                    id: message_id,
                    sender: String::new(),
                    recipient: String::new(),
                    body: body.into_bytes(),
                    encoding: mail::PayloadEncoding::Text,
                    reply_to,
                    intent: mail::DeliveryIntent { wake, notify },
                },
                to,
            ),
            Request::Inbox {
                auth,
                peek,
                wait_secs,
            } => {
                if wait_secs != 0 {
                    self.authorize(connection, &auth)?;
                    return Err(EngineError::AsyncRequired);
                }
                self.inbox(connection, auth, peek)
            }
            Request::StopWaiting { auth } => self.stop_waiting(connection, &auth),
            Request::Unread { auth } => {
                self.authorize(connection, &auth)?;
                Ok(Outcome::Unread {
                    count: self.mail.unread(&mailbox_address(&auth.conversation))?,
                })
            }
            Request::Delivered { auth, attempt_id } => {
                self.finish(connection, auth, attempt_id, true)
            }
            Request::PushFailed { auth, attempt_id } => {
                self.finish(connection, auth, attempt_id, false)
            }
            Request::Shutdown { auth } => {
                self.authorize(connection, &auth)?;
                Ok(Outcome::Shutdown)
            }
        }
    }
    fn now(&self) -> Result<i64, EngineError> {
        let now = (self.clock)()?;
        if now < 0 {
            return Err(EngineError::Clock);
        }
        Ok(now)
    }
    fn ensure_connection(&mut self, id: ConnectionId) -> Result<(), EngineError> {
        if !self.connections.contains_key(&id) {
            if self.connections.len() >= MAX_CONNECTIONS {
                return Err(EngineError::Limit);
            }
            let mut nonce = [0; 32];
            getrandom::fill(&mut nonce).map_err(|_| EngineError::Entropy)?;
            self.connections.insert(
                id,
                Connection {
                    scope: format!("ipc:{}", data_encoding::HEXLOWER.encode(&nonce)),
                },
            );
        }
        Ok(())
    }
    fn authorize(&self, connection: ConnectionId, auth: &Auth) -> Result<(), EngineError> {
        self.registry.registry()?.authorize_connection(
            &auth.conversation.native(),
            &auth.secret(),
            connection,
        )?;
        Ok(())
    }
    /// Mail release precedes registry unbinding. On any storage uncertainty the
    /// engine fails closed until reopened. Other connections' scopes are untouched.
    pub fn disconnect(&mut self, id: ConnectionId) -> Result<(), EngineError> {
        let result = self.disconnect_inner(id);
        if matches!(result, Err(EngineError::StorageUncertain)) {
            self.uncertain = true;
        }
        result
    }
    fn disconnect_inner(&mut self, id: ConnectionId) -> Result<(), EngineError> {
        if self.uncertain {
            return Err(EngineError::StorageUncertain);
        }
        let Some(connection) = self.connections.get(&id) else {
            return Ok(());
        };
        self.mail.disconnect(&connection.scope)?;
        self.registry.transact(|r| {
            r.disconnect(id);
            Ok(())
        })?;
        self.connections.remove(&id);
        self.changed.notify_waiters();
        Ok(())
    }
}
/// Collision-resistant stable address of the full length-delimited identity.
/// Repository is registration metadata, not part of a retained conversation ID.
pub fn mailbox_address(conversation: &Conversation) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"foundry-mailbox-v1\0");
    for field in [
        &conversation.person,
        &conversation.machine,
        &conversation.name,
    ] {
        hasher.update(&(field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    format!("m1:{}", hasher.finalize().to_hex())
}

#[path = "daemon_engine/output.rs"]
mod output;
pub use output::{InboxOutput, InboxSummary, MAX_INBOX_REPLY_BYTES};
