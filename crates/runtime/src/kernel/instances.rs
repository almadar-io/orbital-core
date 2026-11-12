//! An entity's authored `instances` as stored rows.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::schema::{FieldDefinition, FieldType};
use orbital_core_base::value::Value;
use orbital_core_base::value::ValueMap;

fn is_blank(value: &Value) -> bool {
    value.is_null() || value.as_str().is_some_and(str::is_empty)
}

/// An entity's authored `instances` as stored rows: an id-less instance takes the next
/// `<Entity> Id N`, and absent timestamps are `now` in the representation the declared field type asks for:
/// epoch milliseconds for `number`/`int`, the calendar day for `date`, ISO text otherwise. An undeclared timestamp field gets no stamp.
pub fn instance_rows(
    entity_name: &str,
    fields: &[FieldDefinition],
    instances: &[orbital_core_base::schema::sexpr::SObject],
    now_ms: f64,
) -> Vec<Value> {
    let mut minted = 0;
    instances
        .iter()
        .map(|instance| {
            let mut row: ValueMap = instance
                .iter()
                .map(|(key, sexpr)| (key.to_string().into(), super::kernel::sexpr_to_value(sexpr)))
                .collect();
            if row.get("id").is_none_or(is_blank) {
                minted += 1;
                row.insert(
                    "id".into(),
                    Value::string(mock_row_id(entity_name, minted)),
                );
            }
            for key in ["createdAt", "updatedAt"] {
                let declared = fields
                    .iter()
                    .find(|f| f.name.as_deref() == Some(key))
                    .map(|f| &f.field_type);
                if let Some(declared) = declared.filter(|_| row.get(key).is_none_or(is_blank)) {
                    let stamp = match declared {
                        FieldType::Number | FieldType::Int => Value::from(now_ms),
                        FieldType::Date => Value::string(crate::mock_random::date_from_ms(now_ms)),
                        _ => Value::string(crate::mock_random::iso_from_ms(now_ms)),
                    };
                    row.insert(key.to_string().into(), stamp);
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
