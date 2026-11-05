//! Named tool profiles selected by a session's `tool_profile` reference. The
//! table is fixed in code, so a name always selects the same grants. Profile and
//! tool names are provisional until the naming decision settles them.
use crate::{Grants, Result, ToolError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolProfile {
    name: &'static str,
    grants: Grants,
}

/// Every known profile, sorted by name.
const PROFILES: [ToolProfile; 2] = [
    ToolProfile {
        name: "default",
        grants: Grants {
            read: true,
            list: true,
            create: true,
            overwrite: true,
        },
    },
    ToolProfile {
        name: "default/read-only",
        grants: Grants {
            read: true,
            list: true,
            create: false,
            overwrite: false,
        },
    },
];

impl ToolProfile {
    pub fn all() -> &'static [ToolProfile] {
        &PROFILES
    }

    /// Exact, case-sensitive lookup; an unknown name never falls back to another profile.
    pub fn named(name: &str) -> Result<Self> {
        PROFILES
            .iter()
            .find(|profile| profile.name == name)
            .copied()
            .ok_or(ToolError::Invalid("unknown tool profile"))
    }

    /// Grants for an optional profile reference. No reference grants nothing.
    pub fn select(reference: Option<&str>) -> Result<Grants> {
        reference.map_or(Ok(Grants::default()), |name| {
            Self::named(name).map(|profile| profile.grants)
        })
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn grants(&self) -> Grants {
        self.grants
    }

    /// Tools these grants enable, in a fixed order.
    pub fn tools(&self) -> Vec<&'static str> {
        let grants = self.grants;
        [
            ("read", grants.read),
            ("list", grants.list),
            ("write", grants.create || grants.overwrite),
            ("edit", grants.read && grants.overwrite),
        ]
        .into_iter()
        .filter_map(|(tool, enabled)| enabled.then_some(tool))
        .collect()
    }
}
