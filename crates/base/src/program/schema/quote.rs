//! `(quote x)` — an S-expression held as data (G-CROSS-041).
//!
//! The IR is `["quote", <body>]`, where the body is canonical JSON of `x`
//! (object keys sorted, integral numbers without a fraction) with every `@`
//! written as the JSON escape `@`. The body carries no raw `@`, so no
//! binding interpolation, scan or rewrite can reach inside it, and any JSON
//! parser restores `x`. `@almadar/core`'s `encodeQuoteBody` produces the same
//! bytes (shared fixture `fixtures/quote/cases.json`).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::schema::sexpr::SExpression;

/// The special form that holds an S-expression as data.
pub const QUOTE_HEAD: &str = "quote";

/// Largest integer a JSON number carries exactly (2^53).
const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0;

fn canonical(expr: &SExpression) -> serde_json::Value {
    match expr {
        SExpression::Null => serde_json::Value::Null,
        SExpression::Bool(b) => serde_json::Value::Bool(*b),
        SExpression::Number(n) if n.fract() == 0.0 && n.abs() < MAX_EXACT_INT => {
            serde_json::Value::from(*n as i64)
        }
        SExpression::Number(n) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        SExpression::String(s) => serde_json::Value::String(s.clone().into_inner()),
        SExpression::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical).collect())
        }
        SExpression::Object(map) => {
            let mut keys: Vec<&crate::value::Shared<String>> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(key.to_string(), canonical(&map[key]));
            }
            serde_json::Value::Object(out)
        }
    }
}

/// The IR body of `(quote expr)`.
pub fn encode_quote_body(expr: &SExpression) -> String {
    serde_json::to_string(&canonical(expr))
        .expect("an SExpression always serializes")
        .replace('@', "\\u0040")
}

/// Decode a `(quote x)` body back to `x`; `None` when the body is not JSON.
pub fn decode_quote_body(body: &str) -> Option<SExpression> {
    serde_json::from_str(body).ok()
}

/// The IR call `["quote", <body>]` for `expr`.
pub fn quote_expr(expr: &SExpression) -> SExpression {
    SExpression::Array(vec![
        SExpression::string(QUOTE_HEAD.to_string()),
        SExpression::string(encode_quote_body(expr)),
    ].into())
}

/// The encoded body when `expr` is a `(quote x)` call.
pub fn quote_body_of(expr: &SExpression) -> Option<&str> {
    match expr {
        SExpression::Array(items) if items.len() == 2 => match (&items[0], &items[1]) {
            (SExpression::String(head), SExpression::String(body)) if head == QUOTE_HEAD => {
                Some(body)
            }
            _ => None,
        },
        _ => None,
    }
}

/// The quoted data when `value` (an IR literal, e.g. a config default) is a
/// `(quote x)` call, so a literal emitter writes `x` rather than the call.
pub fn quoted_oir_value(value: &crate::ir::types::OirValue) -> Option<crate::ir::types::OirValue> {
    use crate::ir::types::OirValue;
    match value {
        OirValue::Array(items) if items.len() == 2 => match (&items[0], &items[1]) {
            (OirValue::String(head), OirValue::String(body)) if head == QUOTE_HEAD => {
                serde_json::from_str(body).ok()
            }
            _ => None,
        },
        _ => None,
    }
}

/// `(quasiquote x)` — a quoted template whose `(unquote e)` holes are live.
pub const QUASIQUOTE_HEAD: &str = "quasiquote";
/// A live hole inside a quasiquote template.
pub const UNQUOTE_HEAD: &str = "unquote";

/// Why a quasiquote template is malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuasiquoteError {
    /// A `(quasiquote …)` inside the template.
    Nested,
    /// An `(unquote …)` inside a hole.
    UnquoteInUnquote,
    /// `(unquote)` with no expression or more than one.
    UnquoteArity,
}

impl std::fmt::Display for QuasiquoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nested => write!(f, "quasiquote: nested quasiquote is not supported"),
            Self::UnquoteInUnquote => write!(f, "quasiquote: unquote inside an unquote"),
            Self::UnquoteArity => write!(f, "quasiquote: (unquote e) takes exactly one expression"),
        }
    }
}

fn headed<'a>(expr: &'a SExpression, head: &str) -> Option<&'a [SExpression]> {
    match expr {
        SExpression::Array(items) if matches!(items.first(), Some(SExpression::String(h)) if h == head) => {
            Some(items)
        }
        _ => None,
    }
}

fn contains_head(expr: &SExpression, head: &str) -> bool {
    if headed(expr, head).is_some() {
        return true;
    }
    match expr {
        SExpression::Array(items) => items.iter().any(|e| contains_head(e, head)),
        SExpression::Object(map) => map.values().any(|e| contains_head(e, head)),
        _ => false,
    }
}

fn extract_holes(
    expr: &SExpression,
    holes: &mut Vec<SExpression>,
) -> Result<SExpression, QuasiquoteError> {
    if headed(expr, QUASIQUOTE_HEAD).is_some() {
        return Err(QuasiquoteError::Nested);
    }
    if let Some(items) = headed(expr, UNQUOTE_HEAD) {
        if items.len() != 2 {
            return Err(QuasiquoteError::UnquoteArity);
        }
        if contains_head(&items[1], UNQUOTE_HEAD) {
            return Err(QuasiquoteError::UnquoteInUnquote);
        }
        holes.push(items[1].clone());
        return Ok(SExpression::Array(vec![
            SExpression::string(UNQUOTE_HEAD.to_string()),
            SExpression::Number((holes.len() - 1) as f64),
        ].into()));
    }
    match expr {
        SExpression::Array(items) => Ok(SExpression::Array(
            items
                .iter()
                .map(|e| extract_holes(e, holes))
                .collect::<Result<_, _>>()?,
        )),
        SExpression::Object(map) => {
            // Canonical key order, so hole numbering matches the body bytes on every path.
            let mut keys: Vec<&crate::value::Shared<String>> = map.keys().collect();
            keys.sort();
            let mut out = crate::schema::sexpr::SObject::default();
            for key in keys {
                out.insert(key.clone(), extract_holes(&map[key], holes)?);
            }
            Ok(SExpression::object(out))
        }
        other => Ok(other.clone()),
    }
}

/// The IR call `["quasiquote", <body>, hole0, hole1, …]` for `template`: a
/// quote body in which each `(unquote e)` became the marker `["unquote", i]`,
/// with hole `i` = `e` left live. `@almadar/core`'s `quasiquoteExpr` produces
/// the same IR (shared fixture `fixtures/quasiquote/cases.json`).
pub fn quasiquote_expr(template: &SExpression) -> Result<SExpression, QuasiquoteError> {
    let mut holes = Vec::new();
    let body = encode_quote_body(&extract_holes(template, &mut holes)?);
    let mut items = vec![
        SExpression::string(QUASIQUOTE_HEAD.to_string()),
        SExpression::string(body),
    ];
    items.extend(holes);
    Ok(SExpression::Array(items.into()))
}

/// The body and holes when `expr` is a `(quasiquote x)` call.
pub fn quasiquote_of(expr: &SExpression) -> Option<(&str, &[SExpression])> {
    match expr {
        SExpression::Array(items) if items.len() >= 2 => match (&items[0], &items[1]) {
            (SExpression::String(head), SExpression::String(body)) if head == QUASIQUOTE_HEAD => {
                Some((body.as_str(), &items[2..]))
            }
            _ => None,
        },
        _ => None,
    }
}
