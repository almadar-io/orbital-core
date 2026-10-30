//! Shared operator helpers.

use orbital_core_base::value::Value;

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
