//! Shared operator helpers.

use orbital_core_base::value::Value;

/// The key no operator writes into an object it builds: a host that copies the result into a plain object would let
/// it reach the prototype. Every key-writing object operator answers null for it the same way.
pub const REFUSED_KEY: &str = "__proto__";

pub fn is_refused_key(key: &str) -> bool {
    key == REFUSED_KEY
}

/// [`is_refused_key`] over every key of a value, at any depth.
pub fn has_refused_key_in(value: &Value) -> bool {
    match value {
        Value::Object(map) => map
            .iter()
            .any(|(key, inner)| is_refused_key(key) || has_refused_key_in(inner)),
        Value::Array(items) => items.iter().any(has_refused_key_in),
        _ => false,
    }
}

/// Structural deep-equality over `Value`. Numbers compare exactly (IEEE `==`).
///
/// Single definition shared by `validate/equals` and `object/equals` (these
/// previously carried byte-identical local copies).
pub fn deep_equals(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| deep_equals(x, y))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).map(|bv| deep_equals(v, bv)).unwrap_or(false))
        }
        _ => false,
    }
}
