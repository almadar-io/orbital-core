//! An entity's authored `instances` as stored rows.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::value::Value;
use orbital_core_base::value::ValueMap;

fn is_blank(value: &Value) -> bool {
    value.as_str().is_none_or(str::is_empty)
}

/// An entity's authored `instances` as stored rows: an id-less instance takes the next
/// `<Entity> Id N`, and absent timestamps are `now`.
pub fn instance_rows(
    entity_name: &str,
    instances: &[orbital_core_base::schema::sexpr::SObject],
    now_iso: &str,
) -> Vec<Value> {
    let mut minted = 0;
    instances
        .iter()
        .map(|instance| {
            let mut row: ValueMap = instance
                .iter()
                .map(|(key, sexpr)| (key.to_string(), super::kernel::sexpr_to_value(sexpr)))
                .collect();
            if row.get("id").is_none_or(is_blank) {
                minted += 1;
                row.insert(
                    "id".to_string(),
                    Value::string(mock_row_id(entity_name, minted)),
                );
            }
            for key in ["createdAt", "updatedAt"] {
                if row.get(key).is_none_or(is_blank) {
                    row.insert(key.to_string(), Value::string(now_iso.to_string()));
                }
            }
            Value::object(row)
        })
        .collect()
}

/// `Person` -> `Person Id 3`: the id an id-less instance takes.
pub fn mock_row_id(label: &str, n: usize) -> String {
    let mut chars = label.chars();
    let head: String = chars
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    format!("{head}{} Id {n}", chars.as_str())
}
