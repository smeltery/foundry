//! Opaque identifiers. They are only compared, never parsed or derived from
//! credential bytes.

macro_rules! identifier {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier!(
    /// A workspace: the unit every credential is bound to.
    WorkspaceId
);
identifier!(
    /// A user or service principal.
    PrincipalId
);
identifier!(
    /// A machine registered in one workspace.
    MachineId
);
