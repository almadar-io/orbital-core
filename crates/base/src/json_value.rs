use crate::value::Value;


/// JS-semantics conversion for stringify: integer-valued numbers serialize
/// without a decimal point (`JSON.stringify(1)` → `"1"`, never `"1.0"`),
/// matching `to_string_value`'s integer rule. A lambda has no JSON form (JS
/// `JSON.stringify(fn)` → undefined) and serializes as null.
pub fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Number(n) => {
            if n.fract() == 0.0 && n.abs() < i64::MAX as f64 {
                serde_json::Value::Number((*n as i64).into())
            } else {
                serde_json::Number::from_f64(*n)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            }
        }
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Array(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect(),
        ),
        Value::Lambda { .. } => serde_json::Value::Null,
    }
}