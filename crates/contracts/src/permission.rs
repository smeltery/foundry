//! Pure permission state owned by an authoritative caller, never an adapter frame.
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionProfile {
    Current,
    Legacy,
}
#[derive(Clone, PartialEq, Eq)]
pub struct PermissionBinding {
    pub session_id: String,
    pub attempt_id: String,
    pub call_id: String,
    pub tool: String,
    pub input_digest: [u8; 32],
    pub profile: PermissionProfile,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
    Pending,
    Unsupported,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionState {
    Pending,
    Allowed,
    Issued,
    Consumed,
    Denied,
    Unsupported,
    Cancelled,
    Expired,
    Stale,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionResult {
    Applied(PermissionState),
    AlreadyDecided(PermissionState),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingMode {
    Interactive,
    ActionRequired,
    Unsupported,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDisposition {
    Pending,
    ActionRequired,
    Unsupported,
    Allowed,
    Denied,
    Consumed,
    Cancelled,
    Expired,
    Stale,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PermissionError {
    #[error("invalid permission binding")]
    InvalidBinding,
    #[error("invalid permission expiry")]
    InvalidExpiry,
    #[error("permission binding mismatch")]
    BindingMismatch,
    #[error("foreign permission permit")]
    ForeignPermit,
    #[error("permission expired")]
    Expired,
    #[error("permission cancelled")]
    Cancelled,
    #[error("permission is stale")]
    Stale,
    #[error("permission clock moved backwards")]
    TimeRegression,
    #[error("conflicting permission decision")]
    ConflictingDecision,
    #[error("invalid permission state")]
    InvalidState,
}

/// Non-transferable between requests; deliberately not cloneable or serializable.
pub struct PermissionPermit {
    owner: Arc<()>,
    binding: PermissionBinding,
}
pub struct PermissionRequest {
    owner: Arc<()>,
    binding: PermissionBinding,
    expires_at: u64,
    last_now: u64,
    state: PermissionState,
    decision: Option<PermissionDecision>,
}
impl PermissionRequest {
    pub fn new(
        binding: PermissionBinding,
        expires_at: u64,
        now: u64,
    ) -> Result<Self, PermissionError> {
        if [
            &binding.session_id,
            &binding.attempt_id,
            &binding.call_id,
            &binding.tool,
        ]
        .iter()
        .any(|id| id.trim().is_empty())
        {
            return Err(PermissionError::InvalidBinding);
        }
        if expires_at <= now {
            return Err(PermissionError::InvalidExpiry);
        }
        Ok(Self {
            owner: Arc::new(()),
            binding,
            expires_at,
            last_now: now,
            state: PermissionState::Pending,
            decision: None,
        })
    }
    pub fn binding(&self) -> &PermissionBinding {
        &self.binding
    }
    pub fn state(&self) -> PermissionState {
        self.state
    }
    pub fn decide(
        &mut self,
        expected: &PermissionBinding,
        decision: PermissionDecision,
        now: u64,
    ) -> Result<DecisionResult, PermissionError> {
        self.check(expected, now)?;
        if let Some(previous) = self.decision {
            return if previous == decision {
                Ok(DecisionResult::AlreadyDecided(self.state))
            } else {
                Err(PermissionError::ConflictingDecision)
            };
        }
        self.state = match decision {
            PermissionDecision::Pending => return Ok(DecisionResult::Applied(self.state)),
            PermissionDecision::Allow => PermissionState::Allowed,
            PermissionDecision::Deny => PermissionState::Denied,
            PermissionDecision::Unsupported => PermissionState::Unsupported,
        };
        self.decision = Some(decision);
        Ok(DecisionResult::Applied(self.state))
    }
    pub fn take_permit(
        &mut self,
        expected: &PermissionBinding,
        now: u64,
    ) -> Result<PermissionPermit, PermissionError> {
        self.check(expected, now)?;
        if self.state != PermissionState::Allowed {
            return Err(PermissionError::InvalidState);
        }
        self.state = PermissionState::Issued;
        Ok(PermissionPermit {
            owner: Arc::clone(&self.owner),
            binding: self.binding.clone(),
        })
    }
    /// Must succeed immediately before the trusted consumer admits the bound call.
    pub fn consume(
        &mut self,
        permit: PermissionPermit,
        current: &PermissionBinding,
        now: u64,
    ) -> Result<(), PermissionError> {
        if !Arc::ptr_eq(&self.owner, &permit.owner) {
            return Err(PermissionError::ForeignPermit);
        }
        self.check(current, now)?;
        if self.state != PermissionState::Issued || permit.binding != self.binding {
            return Err(PermissionError::InvalidState);
        }
        self.state = PermissionState::Consumed;
        Ok(())
    }
    pub fn cancel(&mut self) {
        if self.state != PermissionState::Consumed {
            self.state = PermissionState::Cancelled;
        }
    }
    pub fn invalidate(&mut self) {
        if self.state != PermissionState::Consumed {
            self.state = PermissionState::Stale;
        }
    }
    pub fn disposition(
        &mut self,
        mode: PendingMode,
        now: u64,
    ) -> Result<PermissionDisposition, PermissionError> {
        self.refresh(now)?;
        Ok(match self.state {
            PermissionState::Pending => match mode {
                PendingMode::Interactive => PermissionDisposition::Pending,
                PendingMode::ActionRequired => PermissionDisposition::ActionRequired,
                PendingMode::Unsupported => PermissionDisposition::Unsupported,
            },
            PermissionState::Allowed | PermissionState::Issued => PermissionDisposition::Allowed,
            PermissionState::Consumed => PermissionDisposition::Consumed,
            PermissionState::Denied => PermissionDisposition::Denied,
            PermissionState::Unsupported => PermissionDisposition::Unsupported,
            PermissionState::Cancelled => PermissionDisposition::Cancelled,
            PermissionState::Expired => PermissionDisposition::Expired,
            PermissionState::Stale => PermissionDisposition::Stale,
        })
    }
    fn refresh(&mut self, now: u64) -> Result<(), PermissionError> {
        if now < self.last_now {
            return Err(PermissionError::TimeRegression);
        }
        self.last_now = now;
        if now >= self.expires_at
            && !matches!(
                self.state,
                PermissionState::Consumed | PermissionState::Cancelled | PermissionState::Stale
            )
        {
            self.state = PermissionState::Expired;
        }
        Ok(())
    }
    fn check(&mut self, current: &PermissionBinding, now: u64) -> Result<(), PermissionError> {
        self.refresh(now)?;
        match self.state {
            PermissionState::Cancelled => return Err(PermissionError::Cancelled),
            PermissionState::Expired => return Err(PermissionError::Expired),
            PermissionState::Stale => return Err(PermissionError::Stale),
            _ => {}
        }
        if current != &self.binding {
            self.invalidate();
            return Err(PermissionError::BindingMismatch);
        }
        Ok(())
    }
}
