//! OIR values, the map aliases and module metadata.

use super::*;

/// Name -> minted id, sorted by name (deterministic IR wire).
pub type OirIdMap = IndexMap<String, String>;

/// Config knob -> its declared type, in authored order.
pub type OirConfigSchema = IndexMap<String, OirConfigFieldType>;

/// Named types in authored order (deterministic codegen and IR wire).
pub type OirTypeMap = IndexMap<String, OirType>;

/// An IR object: keys in authored order, so codegen and the IR wire are deterministic.
pub type OirObject = IndexMap<String, OirValue>;

/// Universal value type
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OirValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<OirValue>),
    Object(OirObject),
}

impl OirValue {
    /// Check if value is null
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Try to get as bool
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Try to get as number
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Try to get as string
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// Try to get as array
    pub fn as_array(&self) -> Option<&[OirValue]> {
        match self {
            Self::Array(arr) => Some(arr),
            _ => None,
        }
    }

    /// Try to get as object
    pub fn as_object(&self) -> Option<&OirObject> {
        match self {
            Self::Object(obj) => Some(obj),
            _ => None,
        }
    }
}

impl From<bool> for OirValue {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<f64> for OirValue {
    fn from(n: f64) -> Self {
        Self::Number(n)
    }
}

impl From<i64> for OirValue {
    fn from(n: i64) -> Self {
        Self::Number(n as f64)
    }
}

impl From<String> for OirValue {
    fn from(s: String) -> Self {
        Self::String(s)
    }
}

impl From<&str> for OirValue {
    fn from(s: &str) -> Self {
        Self::String(s.to_string())
    }
}

impl<T: Into<OirValue>> From<Vec<T>> for OirValue {
    fn from(arr: Vec<T>) -> Self {
        Self::Array(arr.into_iter().map(Into::into).collect())
    }
}

/// Compilation metadata
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OirMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compiler_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

// `to_snake_case` is the canonical `crate::str_utils::to_snake_case` (imported
// at module top) — the byte-identical local copy was removed (Pillar F3).

/// Add every service a `call-service` effect inside `e` names to `names`.
pub fn invoked_services_in(
    e: &crate::ir::expr::OirExpr,
    names: &mut std::collections::BTreeSet<String>,
) {
    use {crate::ir::effects::OirEffect, crate::ir::expr::OirExpr};
    if let OirExpr::Effect { effect } = e {
        if let OirEffect::CallService { service, .. } = effect.as_ref() {
            names.insert(service.clone());
        }
    }
    for child in e.child_exprs() {
        invoked_services_in(child, names);
    }
}
