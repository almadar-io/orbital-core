use super::*;
use crate::schema::types::FieldDefinition;

fn tuple_field() -> FieldDefinition {
    serde_json::from_value(serde_json::json!({
        "name": "position",
        "type": "tuple",
        "properties": { "2": { "type": "string" }, "0": { "type": "number" }, "10": { "type": "boolean" }, "1": { "type": "number" } }
    }))
    .unwrap()
}

/// A tuple's slots ride `properties` keyed by index; they read back in slot order
/// (numeric, so `10` follows `2`) and shape as `OirType::Tuple`.
#[test]
fn a_tuple_field_reads_its_slots_in_order() {
    let f = tuple_field();
    let kinds: Vec<FieldType> = f
        .tuple_slots()
        .iter()
        .map(|s| s.field_type.clone())
        .collect();
    assert_eq!(
        kinds,
        vec![
            FieldType::Number,
            FieldType::Number,
            FieldType::String,
            FieldType::Boolean
        ]
    );
    assert_eq!(
        field_shape(&f),
        OirType::Tuple {
            items: vec![
                OirType::Number,
                OirType::Number,
                OirType::String,
                OirType::Boolean
            ]
        }
    );
}
