//! Pure target negotiation. Availability is not execution or authorization authority.
//! These descriptors do not extend the protocol-1 Ready advertisement.

pub const MAX_REQUIREMENTS: usize = 64;
pub const MAX_ADVERTISEMENTS: usize = 64;
pub const MAX_ID_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Backend,
    Adapter,
    Model,
    Policy,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    Execution,
    MessageReceipt,
    Capture,
    Export,
    InertRestore,
    Continuation,
    NativeValidation,
    PermissionRequests,
    DurableAcceptance,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requirement {
    pub owner: Owner,
    pub capability: Capability,
    pub version: u32,
}
pub struct OperationRequirements<'a> {
    pub operation: &'a str,
    pub version: u32,
    /// Canonical identity binding the complete effective profile, not just its label.
    pub profile: &'a str,
    pub requirements: &'a [Requirement],
}
#[derive(Clone, Copy)]
pub enum Advertisement<'a> {
    Versioned {
        capability: Capability,
        version: u32,
        supported: bool,
    },
    Legacy {
        capability: Capability,
    },
    Unknown {
        name: &'a str,
        version: u32,
    },
}
pub struct OwnerAdvertisement<'a> {
    pub owner: Owner,
    pub operation: &'a str,
    pub version: u32,
    pub profile: Option<&'a str>,
    pub capabilities: &'a [Advertisement<'a>],
}
/// A pure observation; no permission, admission, native approval or permit is issued.
#[derive(Debug, PartialEq, Eq)]
pub struct Availability<'a> {
    pub operation: &'a str,
    pub version: u32,
    pub profile: &'a str,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NegotiationError {
    #[error("invalid capability descriptor")]
    InvalidDescriptor,
    #[error("capability descriptor limit exceeded")]
    Limit,
    #[error("duplicate operation requirement")]
    DuplicateRequirement,
    #[error("duplicate capability owner")]
    DuplicateOwner,
    #[error("duplicate capability advertisement")]
    DuplicateAdvertisement,
    #[error("operation identity mismatch for {0:?}")]
    OperationMismatch(Owner),
    #[error("profile not acknowledged by {0:?}")]
    ProfileNotAcknowledged(Owner),
    #[error("profile identity mismatch for {0:?}")]
    ProfileMismatch(Owner),
    #[error("missing capability owner {0:?}")]
    MissingOwner(Owner),
    #[error("missing capability {capability:?} from {owner:?}")]
    MissingCapability {
        owner: Owner,
        capability: Capability,
    },
    #[error("unversioned capability {capability:?} from {owner:?}")]
    LegacyOnly {
        owner: Owner,
        capability: Capability,
    },
    #[error("incompatible capability version for {capability:?} from {owner:?}")]
    IncompatibleVersion {
        owner: Owner,
        capability: Capability,
    },
    #[error("unsupported capability {capability:?} from {owner:?}")]
    Unsupported {
        owner: Owner,
        capability: Capability,
    },
}

/// Checks the supplied operation's explicit dependencies without effects or fallback.
/// The caller owns completeness of requirements and freshness/authenticity of offers.
pub fn negotiate<'a>(
    operation: &OperationRequirements<'a>,
    owners: &[OwnerAdvertisement<'_>],
) -> Result<Availability<'a>, NegotiationError> {
    validate(operation, owners)?;
    for owner in owners {
        if owner.operation != operation.operation || owner.version != operation.version {
            return Err(NegotiationError::OperationMismatch(owner.owner));
        }
        match owner.profile {
            None => return Err(NegotiationError::ProfileNotAcknowledged(owner.owner)),
            Some(profile) if profile != operation.profile => {
                return Err(NegotiationError::ProfileMismatch(owner.owner));
            }
            Some(_) => {}
        }
    }
    for required in operation.requirements {
        let owner = owners
            .iter()
            .find(|owner| owner.owner == required.owner)
            .ok_or(NegotiationError::MissingOwner(required.owner))?;
        match_requirement(required, owner.capabilities)?;
    }
    Ok(Availability {
        operation: operation.operation,
        version: operation.version,
        profile: operation.profile,
    })
}
fn match_requirement(
    required: &Requirement,
    offers: &[Advertisement<'_>],
) -> Result<(), NegotiationError> {
    let mut versioned = false;
    let mut legacy = false;
    for offer in offers {
        match offer {
            Advertisement::Versioned {
                capability,
                version,
                supported,
            } if *capability == required.capability => {
                versioned = true;
                if *version == required.version {
                    return if *supported {
                        Ok(())
                    } else {
                        Err(NegotiationError::Unsupported {
                            owner: required.owner,
                            capability: required.capability,
                        })
                    };
                }
            }
            Advertisement::Legacy { capability } if *capability == required.capability => {
                legacy = true;
            }
            _ => {}
        }
    }
    let owner = required.owner;
    let capability = required.capability;
    Err(if versioned {
        NegotiationError::IncompatibleVersion { owner, capability }
    } else if legacy {
        NegotiationError::LegacyOnly { owner, capability }
    } else {
        NegotiationError::MissingCapability { owner, capability }
    })
}
fn validate(
    operation: &OperationRequirements<'_>,
    owners: &[OwnerAdvertisement<'_>],
) -> Result<(), NegotiationError> {
    if operation.requirements.len() > MAX_REQUIREMENTS || owners.len() > 4 {
        return Err(NegotiationError::Limit);
    }
    identifier(operation.operation)?;
    identifier(operation.profile)?;
    if operation.version == 0 || operation.requirements.is_empty() {
        return Err(NegotiationError::InvalidDescriptor);
    }
    for (index, requirement) in operation.requirements.iter().enumerate() {
        if requirement.version == 0 {
            return Err(NegotiationError::InvalidDescriptor);
        }
        if operation.requirements[..index].iter().any(|previous| {
            previous.owner == requirement.owner && previous.capability == requirement.capability
        }) {
            return Err(NegotiationError::DuplicateRequirement);
        }
    }
    for (index, owner) in owners.iter().enumerate() {
        if owners[..index]
            .iter()
            .any(|previous| previous.owner == owner.owner)
        {
            return Err(NegotiationError::DuplicateOwner);
        }
        identifier(owner.operation)?;
        if owner.version == 0 {
            return Err(NegotiationError::InvalidDescriptor);
        }
        if let Some(profile) = owner.profile {
            identifier(profile)?;
        }
        if owner.capabilities.len() > MAX_ADVERTISEMENTS {
            return Err(NegotiationError::Limit);
        }
        for (offer_index, offer) in owner.capabilities.iter().enumerate() {
            match offer {
                Advertisement::Versioned { version: 0, .. }
                | Advertisement::Unknown { version: 0, .. } => {
                    return Err(NegotiationError::InvalidDescriptor);
                }
                Advertisement::Unknown { name, .. } => identifier(name)?,
                _ => {}
            }
            if owner.capabilities[..offer_index]
                .iter()
                .any(|previous| duplicate(previous, offer))
            {
                return Err(NegotiationError::DuplicateAdvertisement);
            }
        }
    }
    Ok(())
}
fn identifier(value: &str) -> Result<(), NegotiationError> {
    if value.len() > MAX_ID_BYTES {
        return Err(NegotiationError::Limit);
    }
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(&b))
    {
        return Err(NegotiationError::InvalidDescriptor);
    }
    Ok(())
}
fn duplicate(left: &Advertisement<'_>, right: &Advertisement<'_>) -> bool {
    match (left, right) {
        (
            Advertisement::Versioned {
                capability: a,
                version: av,
                ..
            },
            Advertisement::Versioned {
                capability: b,
                version: bv,
                ..
            },
        ) => a == b && av == bv,
        (Advertisement::Legacy { capability: a }, Advertisement::Legacy { capability: b }) => {
            a == b
        }
        (
            Advertisement::Unknown {
                name: a,
                version: av,
            },
            Advertisement::Unknown {
                name: b,
                version: bv,
            },
        ) => a == b && av == bv,
        _ => false,
    }
}
