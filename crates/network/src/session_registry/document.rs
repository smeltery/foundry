use super::{DurableError, StoredResult};
use crate::session_registry::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Component, PathBuf},
};

pub(super) const MAX_BYTES: usize = 4 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    sessions: Vec<Record>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    person: String,
    machine: String,
    name: String,
    repository: String,
    share_path: bool,
    credential: [u8; 32],
    status: String,
    doing: String,
}
pub(super) fn encode(registry: &SessionRegistry) -> StoredResult<Vec<u8>> {
    let sessions = registry
        .sessions
        .iter()
        .map(|s| {
            let r = &s.registration;
            Ok(Record {
                person: r.conversation.person.clone(),
                machine: r.conversation.machine.clone(),
                name: r.conversation.name.clone(),
                repository: r.repository.to_str().ok_or(DurableError::Corrupt)?.into(),
                share_path: r.share_path,
                credential: s.credential.0,
                status: s.status.clone(),
                doing: s.doing.clone(),
            })
        })
        .collect::<StoredResult<Vec<_>>>()?;
    let bytes = serde_json::to_vec(&Document { sessions }).map_err(|_| DurableError::Corrupt)?;
    if bytes.len() > MAX_BYTES {
        return Err(DurableError::Registry(RegistryError::Limit));
    }
    Ok(bytes)
}
pub(super) fn decode(bytes: &[u8], policy: FolderPolicy) -> StoredResult<SessionRegistry> {
    if bytes.len() > MAX_BYTES {
        return Err(DurableError::Corrupt);
    }
    let document: Document = serde_json::from_slice(bytes).map_err(|_| DurableError::Corrupt)?;
    if document.sessions.len() > MAX_SESSIONS {
        return Err(DurableError::Corrupt);
    }
    let mut registry = SessionRegistry::new(policy);
    let mut identities = HashSet::new();
    let mut credentials = HashSet::new();
    for r in document.sessions {
        for value in [&r.person, &r.machine, &r.name, &r.repository] {
            types::text(value).map_err(|_| DurableError::Corrupt)?;
        }
        for value in [&r.status, &r.doing] {
            if !value.is_empty() {
                types::text(value).map_err(|_| DurableError::Corrupt)?;
            }
        }
        let repository = PathBuf::from(&r.repository);
        if !repository.is_absolute()
            || repository
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            || !identities.insert((r.person.clone(), r.machine.clone(), r.name.clone()))
            || !credentials.insert(r.credential)
        {
            return Err(DurableError::Corrupt);
        }
        // Do not discard offline identities for missing/excluded folders. Existing
        // runtime policy revalidates canonical folders before visibility or use.
        registry.sessions.push(Session {
            registration: Registration {
                conversation: Conversation {
                    person: r.person,
                    machine: r.machine,
                    name: r.name,
                },
                repository,
                share_path: r.share_path,
            },
            credential: SessionCredential(r.credential),
            associations: BTreeMap::new(),
            status: r.status,
            doing: r.doing,
        });
    }
    Ok(registry)
}
