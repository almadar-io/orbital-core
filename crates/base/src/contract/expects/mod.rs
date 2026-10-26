//! `expects` derivation (docs/Almadar_LOLO_Expects_Proposal.md §7): an orbital's consumer-side
//! requirement declarations computed from the full program it sits in, never authored. It walks
//! the program as its `.orb` JSON (policy keys, `ref` traits and `reference` imports as written):
//! `@user.<field>` reads (→ `expects entity [identity]`), relation fields, `persist`/`fetch` targets,
//! cross-orbital `linkedEntity` and import remap targets (→ `expects entity`), `navigate`
//! targets a sibling serves (→ `expects page`), and `listens` on a sibling orbital's trait
//! (→ `expects trait`). Shapes copy the provider's declared fields verbatim.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::ir::expr::{OirBinding, OirBindingRoot};
use crate::route::{extract_static_path_prefix, path_matches_pattern};
use crate::schema::sexpr::SExpression;
use crate::schema::types::ExpectDeclaration;

/// A cross-boundary usage derivation could not turn into a declaration.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExpectationDiagnostic {
    pub kind: DiagnosticKind,
    pub orbital: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiagnosticKind {
    UnknownOrbital,
    IdentityFieldNotDeclared,
    EntityFieldNotDeclared,
    /// A `listens` names an orbital the program does not have; `field` is `Orbital.Trait.EVENT`.
    EventSourceUnknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct DerivedExpectations {
    pub expectations: Vec<ExpectDeclaration>,
    pub diagnostics: Vec<ExpectationDiagnostic>,
}

/// Resolves a behavior by the `uses` path that imports it (`std/behaviors/std-mod-queue`).
pub type BehaviorLoader<'a> = &'a dyn Fn(&str) -> Option<Value>;

fn mismatch(field: &str) -> String {
    format!("expectation shape mismatch — field \"{field}\" has incompatible type constraints")
}

fn base_type(field: &Value) -> Option<&str> {
    field
        .get("type")
        .and_then(Value::as_str)
        .map(|t| if t == "enum" { "string" } else { t })
}

mod merge;
mod imports;
mod walk;
mod derive;
use merge::*;
use imports::*;
use walk::*;
pub use derive::derive_expectations;
pub use merge::merge_expectation_shape;
