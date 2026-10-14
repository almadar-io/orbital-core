//! `T.f` payload fields at dispatch: each takes `f` of a row of `T` the caller has (wrapped in
//! an array for an array field), so a filter or lookup keyed by it matches real data. JS twin:
//! `@almadar-io/verify` `applyProjectedPayload`.

use crate::evaluator::ValueMap;
use crate::schema::{PayloadField, SExpression};
use crate::Value;

/// The values for `fields`' `T.f`-typed members, from `rows_for(T)`'s first row. A field whose
/// `T` has no rows is left out.
pub fn projected_payload<'a>(
    fields: &[PayloadField],
    rows_for: impl Fn(&str) -> Option<&'a [Value]>,
) -> ValueMap {
    let mut out = ValueMap::new();
    for field in fields {
        let Some(projection) = &field.projected_from else { continue };
        let Some(row) = rows_for(&projection.type_name).and_then(|rows| rows.first()) else { continue };
        let value = row.get_path(&projection.field).cloned().unwrap_or(Value::Null);
        let value = if field.field_type.starts_with('[') { Value::Array(vec![value]) } else { value };
        out.insert(field.name.clone(), value);
    }
    out
}

/// Source-payload values for a listen's declared mapping into a target event: a target field
/// typed `T.f` that a mapping entry passes a source field straight into (`tagId: ?id`) makes
/// that source field `f` of `T`'s first row. Computed mapping values are not pass-throughs.
pub fn projected_through_mapping<'a>(
    target_fields: &[PayloadField],
    mapping: &crate::schema::SObject,
    rows_for: impl Fn(&str) -> Option<&'a [Value]>,
) -> ValueMap {
    let projected = projected_payload(target_fields, rows_for);
    let mut out = ValueMap::new();
    for (target_field, expr) in mapping {
        let SExpression::String(binding) = expr else { continue };
        let Some(source_field) = binding.strip_prefix("@payload.") else { continue };
        if source_field.contains('.') {
            continue;
        }
        if let Some(value) = projected.get(target_field) {
            out.insert(source_field.to_string(), value.clone());
        }
    }
    out
}
