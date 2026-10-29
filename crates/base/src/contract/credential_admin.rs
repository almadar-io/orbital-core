//! The credential store operators: raw asks of the host's credential store and mode flag. Every rule over them (the
//! role gate, the declared-name check, masking, the catalog) is the `std-credential-admin` program's.

/// The message of a host that holds no credential store.
pub const NO_STORE: &str = "No credential store is installed on this host";
/// The message of a host with no mode flag.
pub const NO_MANAGER: &str = "No integration manager is installed on this host";

/// One credential store operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CredentialAdminOp {
    #[serde(rename = "credential-list")]
    List,
    #[serde(rename = "credential-set")]
    Set,
    #[serde(rename = "credential-remove")]
    Remove,
    #[serde(rename = "credential-test")]
    Test,
    #[serde(rename = "credential-mode")]
    Mode,
    #[serde(rename = "credential-rotate")]
    Rotate,
}

impl CredentialAdminOp {
    pub const ALL: [CredentialAdminOp; 6] = [Self::List, Self::Set, Self::Remove, Self::Test, Self::Mode, Self::Rotate];

    /// The surface name, `credential-<verb>`.
    pub fn name(self) -> &'static str {
        match self {
            Self::List => "credential-list",
            Self::Set => "credential-set",
            Self::Remove => "credential-remove",
            Self::Test => "credential-test",
            Self::Mode => "credential-mode",
            Self::Rotate => "credential-rotate",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.name() == name)
    }

    /// Whether the operator takes a request object before its options; `rotate` has no parameters, so it takes none.
    pub fn takes_request(self) -> bool {
        !matches!(self, Self::Rotate)
    }
}
