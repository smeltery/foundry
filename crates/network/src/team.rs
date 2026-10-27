//! Full protocol documents and join envelopes, not an admission engine.
//! Deserialization does not verify signatures, proofs, membership or freshness.
//! Secret-bearing types deliberately have no Debug implementation. Serialize only
//! to an explicitly authorized protocol/storage sink, never generic diagnostics.
//!
//! ```compile_fail
//! fn diagnostic<T: std::fmt::Debug>() {}
//! diagnostic::<foundry_network::team::TeamDocument>();
//! ```
//!
//! ```compile_fail
//! fn diagnostic<T: std::fmt::Debug>() {}
//! diagnostic::<foundry_network::team::JoinRequest>();
//! ```
//!
//! ```compile_fail
//! fn diagnostic<T: std::fmt::Debug>() {}
//! diagnostic::<foundry_network::team::JoinReply>();
//! ```
//!
//! ```compile_fail
//! fn diagnostic<T: std::fmt::Debug>() {}
//! diagnostic::<foundry_network::team::LinkRecord>();
//! ```
//!
//! ```compile_fail
//! fn diagnostic<T: std::fmt::Debug>() {}
//! diagnostic::<foundry_network::team::InviteRecord>();
//! ```
//!
//! ```compile_fail
//! fn diagnostic<T: std::fmt::Debug>() {}
//! diagnostic::<foundry_network::signing::SignedRecord<foundry_network::team::LinkRecord>>();
//! ```
use crate::signing::{MemberRecord, MetadataRecord, SignedRecord};
use iroh::EndpointId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Field declaration order is part of the invite signature payload.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteRecord {
    pub hash: String,
    pub expires: i64,
    pub person: Option<String>,
    pub used_by: Option<EndpointId>,
    pub ts: i64,
    #[serde(default)]
    pub reusable: bool,
}

/// A shared bearer secret. A signature alone does not authorize its disclosure.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRecord {
    pub secret: String,
    pub ts: i64,
}

/// Complete wire shape, intentionally distinct from the membership projection.
/// No automatic conversion or persistence promotion is provided.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamDocument {
    pub id: String,
    pub founder: EndpointId,
    pub meta: SignedRecord<MetadataRecord>,
    #[serde(default)]
    pub admissions: BTreeMap<String, SignedRecord<MemberRecord>>,
    pub members: BTreeMap<String, SignedRecord<MemberRecord>>,
    pub invites: BTreeMap<String, SignedRecord<InviteRecord>>,
    #[serde(default)]
    pub link: Option<SignedRecord<LinkRecord>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attestations: BTreeMap<String, String>,
}

/// The transport supplies the authenticated joiner ID; this message cannot.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequest {
    pub team: String,
    pub secret: String,
    pub person: String,
    pub machine: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owner_only: bool,
}

/// A peer's response, not proof that a durable admission or verification occurred.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum JoinReply {
    Ok { state: Box<TeamDocument> },
    Err { error: String },
}
