//! Merging expectation shapes field by field.

use super::*;

pub(super) fn relation_core(relation: &Value) -> Value {
    let mut out = Map::new();
    for key in ["entity", "cardinality", "field", "onDelete"] {
        if let Some(v) = relation.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    Value::Object(out)
}

pub(super) fn sorted_keys(v: Option<&Value>) -> Vec<String> {
    let mut keys: Vec<String> = v
        .and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

pub(super) fn merge_field(prior: &Value, field: &Value) -> Result<Value, String> {
    let name = field
        .get("name")
        .or_else(|| prior.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if base_type(prior) != base_type(field) {
        return Err(mismatch(name));
    }
    let is_relation = prior.get("type") == Some(&Value::from("relation"))
        && field.get("type") == Some(&Value::from("relation"));
    if is_relation {
        let (p, f) = (&prior["relation"], &field["relation"]);
        if let (Some(a), Some(b)) = (p.get("entityId"), f.get("entityId")) {
            if a != b {
                return Err(mismatch(name));
            }
        }
        if relation_core(p) != relation_core(f) {
            return Err(mismatch(name));
        }
    }
    if prior.get("type") == Some(&Value::from("tuple"))
        && field.get("type") == Some(&Value::from("tuple"))
        && sorted_keys(prior.get("properties")) != sorted_keys(field.get("properties"))
    {
        return Err(mismatch(name));
    }
    let num = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64);
    let min = match (num(prior, "min"), num(field, "min")) {
        (None, b) => b,
        (a, None) => a,
        (Some(a), Some(b)) => Some(a.max(b)),
    };
    let max = match (num(prior, "max"), num(field, "max")) {
        (None, b) => b,
        (a, None) => a,
        (Some(a), Some(b)) => Some(a.min(b)),
    };
    if let (Some(lo), Some(hi)) = (min, max) {
        if lo > hi {
            return Err(mismatch(name));
        }
    }
    let mut merged = prior.as_object().cloned().unwrap_or_default();
    if prior.get("required") == Some(&Value::Bool(true))
        || field.get("required") == Some(&Value::Bool(true))
    {
        merged.insert("required".into(), Value::Bool(true));
    }
    let nullable = |v: &Value| v.get("nullable").and_then(Value::as_bool);
    if nullable(prior).is_some() || nullable(field).is_some() {
        merged.insert(
            "nullable".into(),
            Value::Bool(nullable(prior).unwrap_or(true) && nullable(field).unwrap_or(true)),
        );
    }
    let as_number = |n: f64| {
        serde_json::Number::from_f64(n).map(|x| {
            if n.fract() == 0.0 {
                Value::from(n as i64)
            } else {
                Value::Number(x)
            }
        })
    };
    if let Some(v) = min.and_then(as_number) {
        merged.insert("min".into(), v);
    }
    if let Some(v) = max.and_then(as_number) {
        merged.insert("max".into(), v);
    }
    if is_relation && prior["relation"].get("entityId").is_none() {
        if let Some(id) = field["relation"].get("entityId") {
            let mut rel = prior["relation"].as_object().cloned().unwrap_or_default();
            rel.insert("entityId".into(), id.clone());
            merged.insert("relation".into(), Value::Object(rel));
        }
    }
    if prior.get("values").is_some() || field.get("values").is_some() {
        let mut values: Vec<Value> = Vec::new();
        for v in prior
            .get("values")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .chain(
                field
                    .get("values")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten(),
            )
        {
            if !values.contains(v) {
                values.push(v.clone());
            }
        }
        if !values.is_empty() {
            merged.insert("values".into(), Value::Array(values));
        }
    }
    if prior.get("properties").is_some() || field.get("properties").is_some() {
        let mut properties = prior
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (key, value) in field
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let next = match properties.get(key) {
                None => value.clone(),
                Some(existing) => merge_field(existing, value)?,
            };
            properties.insert(key.clone(), next);
        }
        for (declared, constrained) in [(prior, field), (field, prior)] {
            if let Some(additional) = constrained.get("additionalProperties") {
                for key in declared
                    .get("properties")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .map(|(key, _)| key)
                {
                    if constrained
                        .get("properties")
                        .and_then(Value::as_object)
                        .is_some_and(|fixed| fixed.contains_key(key))
                    {
                        continue;
                    }
                    if let Some(fixed) = properties.get(key) {
                        properties.insert(key.clone(), merge_field(fixed, additional)?);
                    }
                }
            }
        }
        merged.insert("properties".into(), Value::Object(properties));
    }
    if prior.get("additionalProperties").is_some() || field.get("additionalProperties").is_some() {
        let additional = match (
            prior.get("additionalProperties"),
            field.get("additionalProperties"),
        ) {
            (Some(left), Some(right)) => merge_field(left, right)?,
            (Some(value), None) | (None, Some(value)) => value.clone(),
            (None, None) => unreachable!(),
        };
        merged.insert("additionalProperties".into(), additional);
    }
    if prior.get("items").is_some() || field.get("items").is_some() {
        let items = match (prior.get("items"), field.get("items")) {
            (None, Some(b)) => b.clone(),
            (Some(a), None) => a.clone(),
            (Some(a), Some(b)) => merge_field(a, b)?,
            (None, None) => Value::Null,
        };
        merged.insert("items".into(), items);
    }
    Ok(Value::Object(merged))
}

/// Union two expectation shapes field by field: requiredness and value lists widen, ranges
/// narrow, nested properties and items merge; incompatible constraints are an error. The
/// composer's merge too (its TS twin, `@almadar/core` `mergeExpectationShape`, is archived).
pub fn merge_expectation_shape(
    existing: Option<&Value>,
    incoming: Option<&Value>,
) -> Result<Option<Value>, String> {
    if existing.is_none() && incoming.is_none() {
        return Ok(None);
    }
    let mut merged: Vec<Value> = existing
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for field in incoming.and_then(Value::as_array).into_iter().flatten() {
        match merged
            .iter()
            .position(|c| c.get("name") == field.get("name"))
        {
            None => merged.push(field.clone()),
            Some(i) => merged[i] = merge_field(&merged[i], field)?,
        }
    }
    Ok(Some(Value::Array(merged)))
}

pub(super) fn as_entity(v: &Value) -> Option<&Value> {
    v.as_object()
        .filter(|o| o.contains_key("fields"))
        .map(|_| v)
}

pub(super) fn inline_entities(o: &Value) -> Vec<&Value> {
    let mut out: Vec<&Value> = o.get("entity").and_then(as_entity).into_iter().collect();
    out.extend(
        o.get("auxiliaryEntities")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(as_entity),
    );
    out
}
