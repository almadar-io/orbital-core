//! Replay a solver model through the real evaluator: a counterexample is reported only when every assumption
//! evaluates truthy and the goal falsy on it, so a disagreement between the decision procedure and the evaluator can
//! never surface as a violation.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::decide::{Model, ModelValue};
use crate::evaluator::context::BindingContextBuilder;
use crate::evaluator::Evaluator;
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::value::{Value, ValueMap};
use std::collections::BTreeMap;

fn to_value(v: &ModelValue) -> Value {
    match v {
        ModelValue::Null => Value::Null,
        ModelValue::Num(r) => Value::Number(r.to_f64()),
        ModelValue::Str(s) => Value::string(s.clone()),
        ModelValue::Bool(b) => Value::Bool(*b),
    }
}

fn insert_path(target: &mut ValueMap, path: &[&str], value: Value) {
    let Some((head, tail)) = path.split_first() else {
        return;
    };
    if tail.is_empty() {
        target.insert(head.to_string(), value);
        return;
    }
    let child = target
        .entry(head.to_string())
        .or_insert_with(|| Value::object(ValueMap::default()));
    if !matches!(child, Value::Object(_)) {
        *child = Value::object(ValueMap::default());
    }
    if let Value::Object(map) = child {
        insert_path(map, tail, value);
    }
}

/// The model's values grouped by binding root (`entity`, `payload`, `__row`, …) as nested records.
pub fn roots(model: &Model) -> BTreeMap<String, ValueMap> {
    let mut out: BTreeMap<String, ValueMap> = BTreeMap::new();
    for (name, value) in &model.values {
        if let (Some(inner), ModelValue::Num(n)) = (
            name.strip_prefix("@len:(array/len @")
                .and_then(|r| r.strip_suffix(')')),
            value,
        ) {
            // The length of a plain bound array: the array itself, that many null elements long.
            let count = n.to_f64();
            if (0.0..=4096.0).contains(&count)
                && count.fract() == 0.0
                && !inner.contains(|c: char| c.is_whitespace() || c == '(')
            {
                let parts: Vec<&str> = inner.split('.').collect();
                if let Some((root, rest)) = parts.split_first() {
                    if !rest.is_empty() {
                        insert_path(
                            out.entry(root.to_string()).or_default(),
                            rest,
                            Value::array(vec![Value::Null; count as usize]),
                        );
                    }
                }
            }
            continue;
        }
        let Some(path) = name.strip_prefix('@') else {
            continue;
        };
        let mut parts = path.split('.');
        let Some(root) = parts.next() else { continue };
        let rest: Vec<&str> = parts.collect();
        let map = out.entry(root.to_string()).or_default();
        if rest.is_empty() {
            continue;
        }
        insert_path(map, &rest, to_value(value));
    }
    out
}

fn reads_unresolved(e: &SExpression) -> bool {
    match e {
        SExpression::Array(items) => {
            matches!(items.first(), Some(SExpression::String(h)) if h == super::obligations::UNRESOLVED)
                || items.iter().any(reads_unresolved)
        }
        _ => false,
    }
}

/// Whether the model makes every assumption truthy and the goal falsy in the evaluator. A value the prover could not
/// model is never confirmed, whatever the evaluator makes of its placeholder.
pub fn confirms(model: &Model, assumptions: &[SExpression], goal: &SExpression) -> bool {
    if reads_unresolved(goal) || assumptions.iter().any(reads_unresolved) {
        return false;
    }
    let mut builder = BindingContextBuilder::new();
    for (root, map) in roots(model) {
        let value = Value::object(map);
        builder = match root.as_str() {
            "entity" => builder.entity(value),
            "payload" => builder.payload(value),
            "user" => builder.user(value),
            "config" => builder.config(value),
            other => builder.local(other, value),
        };
    }
    let ctx = builder.build();
    let evaluator = Evaluator::new();
    let holds = |e: &SExpression| evaluator.evaluate_guard(e, &ctx).ok();
    assumptions.iter().all(|a| holds(a) == Some(true)) && holds(goal) == Some(false)
}

/// The counterexample as JSON for a diagnostic, roots named without the prover's `__` prefix.
pub fn counterexample(model: &Model) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for (root, map) in roots(model)
        .into_iter()
        .filter(|(r, _)| !r.starts_with("len:") && r != "now")
    {
        let name = root.strip_prefix("__").unwrap_or(&root).to_string();
        out.insert(
            name,
            serde_json::to_value(Value::object(map)).unwrap_or(serde_json::Value::Null),
        );
    }
    serde_json::Value::Object(out)
}
