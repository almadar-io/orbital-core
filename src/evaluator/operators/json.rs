//! JSON operators.
//!
//! `(json/parse s Shape)` — pure, deterministic: string → parsed `Value`; `Shape` names the type the value has (checked by `orb validate`, never evaluated). Invalid
//! JSON or non-string input → `Value::Null` (JS `null`), never throws —
//! guards can branch on null. No reviver, no prototype access, no side
//! effects. `(json/stringify v)` is the inverse: any `Value` → JSON string.

use super::macros::*;
use super::*;

fn json_to_value(json: serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => Value::Number(n.as_f64().unwrap_or_default()),
        serde_json::Value::String(s) => Value::String(s),
        serde_json::Value::Array(items) => {
            Value::Array(items.into_iter().map(json_to_value).collect())
        }
        serde_json::Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, json_to_value(v)))
                .collect(),
        ),
    }
}

/// JS-semantics conversion for stringify: integer-valued numbers serialize
/// without a decimal point (`JSON.stringify(1)` → `"1"`, never `"1.0"`),
/// matching `to_string_value`'s integer rule. A lambda has no JSON form (JS
/// `JSON.stringify(fn)` → undefined) and serializes as null.
fn value_to_json(value: &Value) -> serde_json::Value {
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

pub struct JsonParseOp;
impl Operator for JsonParseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "json/parse", >= 2);
        let val = eval_expr(&args[0], ctx)?;
        let Value::String(s) = val else {
            return Ok(Value::Null);
        };
        Ok(serde_json::from_str::<serde_json::Value>(&s)
            .map(json_to_value)
            .unwrap_or(Value::Null))
    }
}

pub struct JsonStringifyOp;
impl Operator for JsonStringifyOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "json/stringify", 1);
        let val = eval_expr(&args[0], ctx)?;
        Ok(Value::String(
            serde_json::to_string(&value_to_json(&val)).unwrap_or_default(),
        ))
    }
}
