//! S-Expression type for guards and effects
//!
//! S-expressions are the core data structure for expressing:
//! - Guards (conditional expressions)
//! - Effects (side effects to execute)
//! - Computed values
//!
//! Examples:
//! - `["and", ["!=", "@payload.email", ""], [">=", "@payload.age", 18]]`
//! - `["persist", "create", "User", "@payload.data"]`
//! - `["render-ui", "main", { "type": "page-header", "title": "Hello" }]`
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;

/// An S-expression object: keys in authored order, so lowering and codegen are deterministic.
pub type SObject = crate::collections::IndexMap<String, SExpression>;

/// S-Expression - the fundamental unit of Orbital logic
///
/// An S-expression can be:
/// - A primitive value (null, bool, number, string)
/// - An array (function call or list)
/// - An object (key-value map)
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(untagged)]
#[derive(Default)]
pub enum SExpression {
    /// Null value
    #[default]
    Null,
    /// Boolean value
    Bool(bool),
    /// Numeric value (integers and floats)
    Number(f64),
    /// String value (may contain @bindings)
    String(crate::value::Shared<String>),
    /// Array - either a function call `[op, ...args]` or a list
    Array(Vec<SExpression>),
    /// Object - key-value map
    Object(SObject),
}

/// Read by one visit per node: the derived `untagged` form buffers every node and retries each variant, which
/// makes parsing a guard cost more than evaluating it.
impl<'de> Deserialize<'de> for SExpression {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(SExpressionVisitor)
    }
}

struct SExpressionVisitor;

impl<'de> serde::de::Visitor<'de> for SExpressionVisitor {
    type Value = SExpression;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an S-expression: null, a boolean, a number, a string, an array or an object")
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<SExpression, E> {
        Ok(SExpression::Null)
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<SExpression, E> {
        Ok(SExpression::Null)
    }

    fn visit_some<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<SExpression, D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<SExpression, E> {
        Ok(SExpression::Bool(value))
    }

    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<SExpression, E> {
        Ok(SExpression::Number(value as f64))
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<SExpression, E> {
        Ok(SExpression::Number(value as f64))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<SExpression, E> {
        Ok(SExpression::Number(value))
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<SExpression, E> {
        Ok(SExpression::string(value))
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<SExpression, E> {
        Ok(SExpression::string(value))
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<SExpression, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(4096));
        while let Some(item) = seq.next_element::<SExpression>()? {
            items.push(item);
        }
        Ok(SExpression::Array(items))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<SExpression, A::Error> {
        let mut object = SObject::with_capacity_and_hasher(map.size_hint().unwrap_or(0).min(4096), Default::default());
        while let Some((key, value)) = map.next_entry::<String, SExpression>()? {
            object.insert(key, value);
        }
        Ok(SExpression::Object(object))
    }
}

/// What [`SExpression::try_rewrite`] does with a node it enters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteVisit {
    /// Rewrite the node's children, then hand the rebuilt node to `exit`.
    Descend,
    /// Keep the node and everything under it as written; `exit` is not called for it.
    Keep,
}

enum RewriteFrame<'a> {
    Enter(&'a SExpression),
    Array(usize),
    Object(Vec<String>),
}

mod methods;
mod walk;
mod convert;
mod json;
use walk::*;
/// A `(list …)` literal marker, stripped; any other array unchanged.
pub fn strip_literal_list_marker(arr: &[SExpression]) -> &[SExpression] {
    match arr.first() {
        Some(SExpression::String(s)) if s == "list" => &arr[1..],
        _ => arr,
    }
}

pub use json::{json_value_leaves, json_value_leaves_mut};
pub use walk::{
    ConditionalNode, MappedChildren, OperatorLookup, OperatorReturn, SequentialBindings, ValueSlot,
    ValueWalk,
};

#[cfg(test)]
mod tests;
