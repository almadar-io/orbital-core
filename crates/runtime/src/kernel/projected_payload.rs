//! `T.f` payload fields at dispatch: each takes `f` of a row of `T` the caller has (wrapped in
//! an array for an array field), so a filter or lookup keyed by it matches real data. JS twin:
//! `@almadar-io/verify` `applyProjectedPayload`.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::value::ValueMap;
use orbital_core_base::schema::types::PayloadField;
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::value::Value;

/// The values for `fields`' `T.f`-typed members, from `rows_for(T)`'s first row. A field whose
/// `T` has no rows is left out.
pub fn projected_payload<'a>(
    fields: &[PayloadField],
    rows_for: impl Fn(&str) -> Option<&'a [Value]>,
) -> ValueMap {
    let mut out = ValueMap::default();
    for field in fields {
        let Some(projection) = &field.projected_from else {
            continue;
        };
        let Some(source_field) = projection.field.as_deref() else {
            continue;
        };
        let Some(row) = rows_for(&projection.type_name).and_then(|rows| rows.first()) else {
            continue;
        };
        let value = row
            .get_path(source_field)
            .cloned()
            .unwrap_or(Value::Null);
        let value = if field.field_type.starts_with('[') {
            Value::array(vec![value])
        } else {
            value
        };
        out.insert(field.name.clone(), value);
    }
    out
}

/// Source-payload values for a listen's declared mapping into a target event: a target field
/// typed `T.f` that a mapping entry passes a source field straight into (`tagId: ?id`) makes
/// that source field `f` of `T`'s first row. Computed mapping values are not pass-throughs.
pub fn projected_through_mapping<'a>(
    target_fields: &[PayloadField],
    mapping: &orbital_core_base::schema::sexpr::SObject,
    rows_for: impl Fn(&str) -> Option<&'a [Value]>,
) -> ValueMap {
    let projected = projected_payload(target_fields, rows_for);
    let mut out = ValueMap::default();
    for (target_field, expr) in mapping {
        let SExpression::String(binding) = expr else {
            continue;
        };
        let Some(source_field) = binding.strip_prefix("@payload.") else {
            continue;
        };
        if source_field.contains('.') {
            continue;
        }
        if let Some(value) = projected.get(target_field) {
            out.insert(source_field.to_string(), value.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_source_provenance_does_not_read_rows_but_real_projection_does() {
        let mut field: PayloadField = serde_json::from_value(serde_json::json!({
            "name":"row","type":"object","projectedFrom":{"type":"Shared"}
        })).unwrap();
        assert!(projected_payload(&[field.clone()], |_| panic!("Named source is not a row projection")).is_empty());
        field.projected_from.as_mut().unwrap().field = Some("label".into());
        let rows = vec![serde_json::from_value(serde_json::json!({"label":"kept"})).unwrap()];
        assert_eq!(projected_payload(&[field], |_| Some(&rows))["row"], Value::string("kept"));
    }
}
