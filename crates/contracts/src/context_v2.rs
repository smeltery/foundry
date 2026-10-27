//! Inert restore/continuation DTOs. Decoding never grants native or attempt authority.
use crate::context::{
    ContextDigest, ContextError, ContextFormat, ContextId, ContextPayload, ContextReceipt,
};
use serde::{Deserialize, Serialize};
#[path = "context_v2_codec.rs"]
mod codec;
pub use codec::{decode_frame, encode_frame, restore_binding};

pub const RESTORE_CAPABILITY: &str = "context_restore_v2";
pub const CONTINUE_CAPABILITY: &str = "context_continue_v2";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadingScope {
    pub restore_id: ContextId,
    pub load_id: ContextReceipt,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreDescriptor {
    pub capture_id: ContextId,
    pub checkpoint_id: ContextId,
    pub record_digest: ContextDigest,
    pub witness_digest: ContextDigest,
    pub format: ContextFormat,
    pub configuration_digest: ContextDigest,
    pub payload_length: u64,
    pub payload_digest: ContextDigest,
    pub retention_binding: ContextDigest,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuationIdentity {
    pub attempt_id: ContextId,
    pub continuation_id: ContextId,
    pub input_id: ContextId,
    pub input_sequence: u64,
    pub input_digest: ContextDigest,
    pub input_bytes: u64,
    pub authorization_binding: ContextDigest,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameKind {
    Begin,
    Prepared,
    Payload,
    Restored,
    Continue,
    Accepted,
}

/// Private framing wrapper, not a native-approval or continuation permit.
/// ```compile_fail
/// use foundry_contracts::context_v2::LoadingFrame;
/// fn log(frame: &LoadingFrame) { println!("{frame:?}"); }
/// ```
/// ```compile_fail
/// use foundry_contracts::context_v2::LoadingFrame;
/// fn persist(frame: &LoadingFrame) { serde_json::to_vec(frame).unwrap(); }
/// ```
pub struct LoadingFrame(Wire);
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Wire {
    #[serde(rename = "restore_begin_v2")]
    Begin {
        scope: LoadingScope,
        descriptor: RestoreDescriptor,
    },
    #[serde(rename = "restore_prepared_v2")]
    Prepared {
        scope: LoadingScope,
        begin_binding: ContextDigest,
    },
    #[serde(rename = "restore_payload_v2")]
    Payload {
        scope: LoadingScope,
        begin_binding: ContextDigest,
        payload_length: u64,
        payload_digest: ContextDigest,
        payload_base64: String,
        receipt: ContextReceipt,
    },
    #[serde(rename = "context_restored_v2")]
    Restored {
        scope: LoadingScope,
        begin_binding: ContextDigest,
        payload_digest: ContextDigest,
        receipt: ContextReceipt,
    },
    #[serde(rename = "continue_context_v2")]
    Continue {
        scope: LoadingScope,
        continuation: ContinuationIdentity,
        text: String,
        receipt: ContextReceipt,
    },
    #[serde(rename = "continuation_accepted_v2")]
    Accepted {
        scope: LoadingScope,
        continuation: ContinuationIdentity,
        receipt: ContextReceipt,
    },
}
impl LoadingFrame {
    pub fn begin(scope: LoadingScope, descriptor: RestoreDescriptor) -> Result<Self, ContextError> {
        Self::checked(Wire::Begin { scope, descriptor })
    }
    pub fn prepared(scope: LoadingScope, begin_binding: ContextDigest) -> Self {
        Self(Wire::Prepared {
            scope,
            begin_binding,
        })
    }
    pub fn payload(
        scope: LoadingScope,
        begin_binding: ContextDigest,
        payload: &ContextPayload,
        receipt: ContextReceipt,
    ) -> Result<Self, ContextError> {
        use base64::{Engine, engine::general_purpose::STANDARD};
        Self::checked(Wire::Payload {
            scope,
            begin_binding,
            payload_length: payload.as_bytes().len() as u64,
            payload_digest: payload.digest(),
            payload_base64: STANDARD.encode(payload.as_bytes()),
            receipt,
        })
    }
    pub fn restored(
        scope: LoadingScope,
        begin_binding: ContextDigest,
        payload_digest: ContextDigest,
        receipt: ContextReceipt,
    ) -> Self {
        Self(Wire::Restored {
            scope,
            begin_binding,
            payload_digest,
            receipt,
        })
    }
    pub fn continuation(
        scope: LoadingScope,
        continuation: ContinuationIdentity,
        text: &str,
        receipt: ContextReceipt,
    ) -> Result<Self, ContextError> {
        codec::input(&continuation, text)?;
        Self::checked(Wire::Continue {
            scope,
            continuation,
            text: text.into(),
            receipt,
        })
    }
    pub fn accepted(
        scope: LoadingScope,
        continuation: ContinuationIdentity,
        receipt: ContextReceipt,
    ) -> Result<Self, ContextError> {
        Self::checked(Wire::Accepted {
            scope,
            continuation,
            receipt,
        })
    }
    fn checked(wire: Wire) -> Result<Self, ContextError> {
        codec::validate(&wire)?;
        Ok(Self(wire))
    }
    pub fn kind(&self) -> FrameKind {
        match &self.0 {
            Wire::Begin { .. } => FrameKind::Begin,
            Wire::Prepared { .. } => FrameKind::Prepared,
            Wire::Payload { .. } => FrameKind::Payload,
            Wire::Restored { .. } => FrameKind::Restored,
            Wire::Continue { .. } => FrameKind::Continue,
            Wire::Accepted { .. } => FrameKind::Accepted,
        }
    }
    pub fn scope(&self) -> &LoadingScope {
        match &self.0 {
            Wire::Begin { scope, .. }
            | Wire::Prepared { scope, .. }
            | Wire::Payload { scope, .. }
            | Wire::Restored { scope, .. }
            | Wire::Continue { scope, .. }
            | Wire::Accepted { scope, .. } => scope,
        }
    }
    /// Pure scope comparison, never a state transition or rebinding operation.
    pub fn check_scope(&self, expected: &LoadingScope) -> Result<(), ContextError> {
        if self.scope() != expected {
            return Err(ContextError::Integrity);
        }
        Ok(())
    }
    pub fn check_prepared(
        &self,
        scope: &LoadingScope,
        binding: ContextDigest,
    ) -> Result<(), ContextError> {
        self.check_scope(scope)?;
        match &self.0 {
            Wire::Prepared { begin_binding, .. } if *begin_binding == binding => Ok(()),
            _ => Err(ContextError::Integrity),
        }
    }
    pub fn check_restored(
        &self,
        scope: &LoadingScope,
        binding: ContextDigest,
        payload: ContextDigest,
        expected_receipt: &ContextReceipt,
    ) -> Result<(), ContextError> {
        self.check_scope(scope)?;
        match &self.0 {
            Wire::Restored {
                begin_binding,
                payload_digest,
                receipt,
                ..
            } if *begin_binding == binding
                && *payload_digest == payload
                && receipt == expected_receipt =>
            {
                Ok(())
            }
            _ => Err(ContextError::Integrity),
        }
    }
    pub fn check_accepted(
        &self,
        scope: &LoadingScope,
        identity: &ContinuationIdentity,
        expected_receipt: &ContextReceipt,
    ) -> Result<(), ContextError> {
        self.check_scope(scope)?;
        match &self.0 {
            Wire::Accepted {
                continuation,
                receipt,
                ..
            } if continuation == identity && receipt == expected_receipt => Ok(()),
            _ => Err(ContextError::Integrity),
        }
    }
    pub fn descriptor(&self) -> Option<&RestoreDescriptor> {
        if let Wire::Begin { descriptor, .. } = &self.0 {
            Some(descriptor)
        } else {
            None
        }
    }
    pub fn continuation_identity(&self) -> Option<&ContinuationIdentity> {
        match &self.0 {
            Wire::Continue { continuation, .. } | Wire::Accepted { continuation, .. } => {
                Some(continuation)
            }
            _ => None,
        }
    }
    pub fn begin_binding(&self) -> Option<ContextDigest> {
        match &self.0 {
            Wire::Prepared { begin_binding, .. }
            | Wire::Payload { begin_binding, .. }
            | Wire::Restored { begin_binding, .. } => Some(*begin_binding),
            _ => None,
        }
    }
    pub fn receipt(&self) -> Option<&ContextReceipt> {
        match &self.0 {
            Wire::Payload { receipt, .. }
            | Wire::Restored { receipt, .. }
            | Wire::Continue { receipt, .. }
            | Wire::Accepted { receipt, .. } => Some(receipt),
            _ => None,
        }
    }
    pub fn text(&self) -> Option<&str> {
        if let Wire::Continue { text, .. } = &self.0 {
            Some(text)
        } else {
            None
        }
    }
    pub fn decode_payload(&self) -> Result<Option<ContextPayload>, ContextError> {
        if let Wire::Payload { payload_base64, .. } = &self.0 {
            codec::payload(payload_base64).map(Some)
        } else {
            Ok(None)
        }
    }
}
