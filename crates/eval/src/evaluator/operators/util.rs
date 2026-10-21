//! Shared operator helpers.

use orbital_core_base::error::{EvalError, OrbitalError};
use orbital_core_base::value::Value;

/// The key no operator writes into an object it builds: a host that copies the result into a plain object would let
/// it reach the prototype. Every key-writing object operator refuses it the same way.
pub const REFUSED_KEY: &str = "__proto__";

pub fn refuse_proto_key(operator: &str, key: &str) -> Result<(), OrbitalError> {
    if key == REFUSED_KEY {
        return Err(OrbitalError::Eval(EvalError::InvalidArgument(format!(
            "{operator}: `{REFUSED_KEY}` is not a permitted object key"
        ))));
    }
    Ok(())
}

/// [`refuse_proto_key`] over every key of a value, at any depth.
pub fn refuse_proto_keys_in(operator: &str, value: &Value) -> Result<(), OrbitalError> {
    match value {
        Value::Object(map) => map.iter().try_for_each(|(key, inner)| {
            refuse_proto_key(operator, key)?;
            refuse_proto_keys_in(operator, inner)
        }),
        Value::Array(items) => items.iter().try_for_each(|item| refuse_proto_keys_in(operator, item)),
        _ => Ok(()),
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
