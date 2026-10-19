//! JSON operators.
//!
//! `(json/parse s Shape)` — pure, deterministic: string → parsed `Value`; `Shape` names the type the value has (checked by `orb validate`, never evaluated). Invalid
//! JSON or non-string input → `Value::Null` (JS `null`), never throws —
//! guards can branch on null. No reviver, no prototype access, no side
//! effects. `(json/stringify v)` is the inverse: any `Value` → JSON string.

use super::macros::*;
use super::*;

fn parse_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(text)
}

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

pub struct JsonParseOp;
impl Operator for JsonParseOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "json/parse", >= 2);
        let val = eval_expr(&args[0], ctx)?;
        let Value::String(s) = val else {
            return Ok(Value::Null);
        };
        Ok(parse_json::<serde_json::Value>(&s)
            .map(json_to_value)
            .unwrap_or(Value::Null))
    }
}

pub struct JsonValidOp;
impl Operator for JsonValidOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        if args.len() != 1 {
            return Err(OrbitalError::Eval(EvalError::InvalidArgument(
                "json/valid: expected exactly one argument".into(),
            )));
        }
        match eval_expr(&args[0], ctx)? {
            Value::String(text) => {
                // Syntax validity must not depend on numeric or Unicode value conversion.
                Ok(Value::Bool(
                    parse_json::<serde::de::IgnoredAny>(&text).is_ok(),
                ))
            }
            other => Err(OrbitalError::Eval(EvalError::TypeMismatch {
                expected: "string".into(),
                actual: other.type_name().into(),
            })),
        }
    }
}

/// `(json/nodes v)`: `v` and every value nested in it, pre-order (a list's items in order, an object's values in key order).
pub struct JsonNodesOp;
impl Operator for JsonNodesOp {
    fn evaluate(&self, args: &[SExpression], ctx: &BindingContext) -> EvalResult {
        require_arity!(args, "json/nodes", == 1);
        let mut nodes = Vec::new();
        let mut stack = vec![eval_expr(&args[0], ctx)?];
        while let Some(value) = stack.pop() {
            match &value {
                Value::Array(items) => stack.extend(items.iter().rev().cloned()),
                Value::Object(map) => stack.extend(map.values().rev().cloned()),
                _ => {}
            }
            nodes.push(value);
        }
        Ok(Value::Array(nodes))
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

#[allow(unused_imports)]
pub(crate) use orbital_core_base::json_value::value_to_json;
