use super::*;
use iroh::EndpointId;
use serde::{Deserialize, Serialize};

/// Random local descriptor ID, not a code hash or derived endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CodeIntentId(pub(super) [u8; 32]);
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlEffect {
    ReconcileCode { id: CodeIntentId, generation: u64 },
    RetireCode { id: CodeIntentId, generation: u64 },
    PublishLatestState { revision: u64, generation: u64 },
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredIntent {
    pub id: CodeIntentId,
    pub code: String,
    pub invitation_hash: String,
    pub expires_ms: i64,
    pub creator: EndpointId,
    pub team: String,
    pub founder: EndpointId,
    pub incarnation: [u8; 32],
    pub policy: [u8; 32],
    pub creation_revision: u64,
    pub issued_ms: i64,
    pub person: Option<String>,
}
/// Opaque precommit candidate. There is no accessor for uncommitted invitation material.
/// ```compile_fail
/// fn debug<T: std::fmt::Debug>() {}
/// debug::<foundry_network::full_team_store::PendingCodeIntent>();
/// ```
pub struct PendingCodeIntent {
    pub(super) transition: PendingTransition,
    pub(super) code: String,
    pub(super) ticket: String,
    pub(super) intent: Option<CodeIntentId>,
    pub(super) expires_ms: i64,
}
/// Privileged output created only after durable commit and sync succeed.
/// ```compile_fail
/// fn debug<T: std::fmt::Debug>() {}
/// debug::<foundry_network::full_team_store::IssuedCodeInvite>();
/// ```
pub struct IssuedCodeInvite {
    pub(super) loaded: LoadedTeam,
    pub(super) code: String,
    pub(super) ticket: String,
    pub(super) intent: Option<CodeIntentId>,
}
impl IssuedCodeInvite {
    pub fn code(&self) -> &str {
        &self.code
    }
    pub fn ticket(&self) -> &str {
        &self.ticket
    }
    pub fn listener_intent(&self) -> Option<CodeIntentId> {
        self.intent
    }
    pub fn loaded(&self) -> &LoadedTeam {
        &self.loaded
    }
}
/// Private generation fence. Revalidate before bind and before publishing it.
/// It cannot authorize draining a consumed invitation's existing join reply.
/// ```compile_fail
/// fn debug<T: std::fmt::Debug>() {}
/// debug::<foundry_network::full_team_store::CodeLease>();
/// ```
pub struct CodeLease {
    pub(super) instance: Arc<StoreInstance>,
    pub(super) id: CodeIntentId,
    pub(super) generation: u64,
    pub(super) expires_ms: i64,
}
impl CodeLease {
    pub fn id(&self) -> CodeIntentId {
        self.id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn expires_ms(&self) -> i64 {
        self.expires_ms
    }
}
