//! Bounded directory with optional protected persistence. No detection or sends.
//! The daemon owns synchronization, trusted person/machine attribution and fresh
//! connection IDs. Credentials authorize only the exact retained conversation.
#[path = "session_registry/durable.rs"]
mod durable;
pub use durable::{DurableError, DurableRegistry, SESSION_DATABASE};
#[path = "session_registry/policy.rs"]
mod policy;
#[path = "session_registry/types.rs"]
mod types;
pub use policy::FolderPolicy;
use policy::directory;
use std::collections::BTreeMap;
pub use types::*;
use types::{identity, text};

#[derive(Clone)]
struct Session {
    registration: Registration,
    credential: SessionCredential,
    associations: BTreeMap<ConnectionId, DeliveryCapability>,
    status: String,
    doing: String,
}

#[derive(Clone)]
pub struct SessionRegistry {
    sessions: Vec<Session>,
    policy: std::sync::Arc<FolderPolicy>,
}
impl SessionRegistry {
    pub fn new(policy: FolderPolicy) -> Self {
        Self {
            sessions: Vec::new(),
            policy: std::sync::Arc::new(policy),
        }
    }

    /// Offline entries consume capacity deliberately; they are never evicted and
    /// their stable names cannot be silently reassigned to another conversation.
    pub fn register(&mut self, mut registration: Registration) -> Result<Registered> {
        identity(&registration.conversation)?;
        registration.repository = directory(&registration.repository)?;
        if !self.policy.permits(&registration.repository) {
            return Err(RegistryError::Excluded);
        }
        if self
            .sessions
            .iter()
            .any(|s| s.registration.conversation == registration.conversation)
        {
            return Err(RegistryError::Unauthorized);
        }
        if self.sessions.len() >= MAX_SESSIONS {
            return Err(RegistryError::Limit);
        }
        let mut secret = [0; 32];
        getrandom::fill(&mut secret).map_err(|_| RegistryError::Entropy)?;
        let credential = SessionCredential(secret);
        let result = Registered {
            conversation: registration.conversation.clone(),
            credential: credential.clone(),
        };
        self.sessions.push(Session {
            registration,
            credential,
            associations: BTreeMap::new(),
            status: String::new(),
            doing: String::new(),
        });
        Ok(result)
    }

    fn authorized(
        &self,
        conversation: &Conversation,
        credential: &SessionCredential,
    ) -> Result<usize> {
        identity(conversation)?;
        self.sessions
            .iter()
            .position(|s| {
                s.registration.conversation == *conversation
                    && constant_time_equal(&s.credential.0, &credential.0)
            })
            .ok_or(RegistryError::Unauthorized)
    }

    /// Re-register or add a multiplexed/hook-only connection using its own secret.
    /// A connection may be associated with many sessions, but grants no authority
    /// over them by itself. Reattachment replaces only this adapter declaration.
    pub fn attach(
        &mut self,
        conversation: &Conversation,
        credential: &SessionCredential,
        connection: ConnectionId,
        capability: DeliveryCapability,
    ) -> Result<()> {
        let index = self.authorized(conversation, credential)?;
        let session = &mut self.sessions[index];
        if !self.policy.permits(&session.registration.repository) {
            return Err(RegistryError::Excluded);
        }
        if !session.associations.contains_key(&connection)
            && session.associations.len() >= MAX_ASSOCIATIONS
        {
            return Err(RegistryError::Limit);
        }
        session.associations.insert(connection, capability);
        Ok(())
    }

    pub fn disconnect(&mut self, connection: ConnectionId) {
        for session in &mut self.sessions {
            session.associations.remove(&connection);
        }
    }

    /// Used by delivery acknowledgement handlers as well as status mutations.
    pub fn authorize_connection(
        &self,
        conversation: &Conversation,
        credential: &SessionCredential,
        connection: ConnectionId,
    ) -> Result<()> {
        let index = self.authorized(conversation, credential)?;
        let session = &self.sessions[index];
        if !self.policy.permits(&session.registration.repository) {
            return Err(RegistryError::Excluded);
        }
        if !session.associations.contains_key(&connection) {
            return Err(RegistryError::Unauthorized);
        }
        Ok(())
    }

    pub fn set_status(
        &mut self,
        conversation: &Conversation,
        credential: &SessionCredential,
        connection: ConnectionId,
        status: &str,
        doing: &str,
    ) -> Result<()> {
        for value in [status, doing] {
            if !value.is_empty() {
                text(value)?;
            }
        }
        self.authorize_connection(conversation, credential, connection)?;
        let index = self.authorized(conversation, credential)?;
        self.sessions[index].status = status.to_owned();
        self.sessions[index].doing = doing.to_owned();
        Ok(())
    }

    /// Replacing policy immediately hides and blocks existing excluded entries.
    pub fn set_policy(&mut self, policy: FolderPolicy) {
        self.policy = std::sync::Arc::new(policy);
    }

    pub fn who(&self, filter: &DirectoryFilter, limit: usize) -> Result<Vec<DirectoryEntry>> {
        if limit == 0 || limit > MAX_RESULTS {
            return Err(RegistryError::Limit);
        }
        let matches = self.matching(filter)?;
        if matches.len() > limit {
            return Err(RegistryError::Limit);
        }
        Ok(matches
            .into_iter()
            .map(|s| {
                let mut capabilities = Vec::new();
                for capability in s.associations.values() {
                    if !capabilities.contains(capability) {
                        capabilities.push(*capability);
                    }
                }
                DirectoryEntry {
                    conversation: s.registration.conversation.clone(),
                    repository: s
                        .registration
                        .share_path
                        .then(|| s.registration.repository.clone()),
                    online: !s.associations.is_empty(),
                    status: s.status.clone(),
                    doing: s.doing.clone(),
                    capabilities,
                }
            })
            .collect())
    }

    /// Exact directory resolution, including retained offline addresses. An empty
    /// association list means queued/offline, never successful adapter delivery.
    pub fn resolve(&self, filter: &DirectoryFilter) -> Result<DeliveryTarget> {
        let matches = self.matching(filter)?;
        match matches.as_slice() {
            [] => Err(RegistryError::NotFound),
            [session] => Ok(target(session)),
            _ => Err(RegistryError::Ambiguous),
        }
    }

    /// Infer association only when this connection owns exactly one visible session.
    pub fn linked(&self, connection: ConnectionId) -> Result<DeliveryTarget> {
        let mut matches = self.sessions.iter().filter(|s| {
            s.associations.contains_key(&connection)
                && self.policy.permits(&s.registration.repository)
        });
        let first = matches.next().ok_or(RegistryError::NotFound)?;
        if matches.next().is_some() {
            return Err(RegistryError::Ambiguous);
        }
        Ok(target(first))
    }

    fn matching(&self, filter: &DirectoryFilter) -> Result<Vec<&Session>> {
        for value in [&filter.person, &filter.machine, &filter.name]
            .into_iter()
            .flatten()
        {
            text(value)?;
        }
        let repository = filter
            .repository
            .as_ref()
            .map(|p| directory(p))
            .transpose()?;
        Ok(self
            .sessions
            .iter()
            .filter(|s| {
                let r = &s.registration;
                self.policy.permits(&r.repository)
                    && (!filter.online_only || !s.associations.is_empty())
                    && filter
                        .person
                        .as_ref()
                        .is_none_or(|v| v == &r.conversation.person)
                    && filter
                        .machine
                        .as_ref()
                        .is_none_or(|v| v == &r.conversation.machine)
                    && filter
                        .name
                        .as_ref()
                        .is_none_or(|v| v == &r.conversation.name)
                    && repository
                        .as_ref()
                        .is_none_or(|v| r.share_path && v == &r.repository)
            })
            .collect())
    }
}
fn target(session: &Session) -> DeliveryTarget {
    DeliveryTarget {
        conversation: session.registration.conversation.clone(),
        associations: session
            .associations
            .iter()
            .map(|(id, capability)| (*id, *capability))
            .collect(),
    }
}
fn constant_time_equal(left: &[u8; 32], right: &[u8; 32]) -> bool {
    let left_key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, left);
    let right_key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, right);
    let signature = ring::hmac::sign(&right_key, b"session credential");
    ring::hmac::verify(&left_key, b"session credential", signature.as_ref()).is_ok()
}
