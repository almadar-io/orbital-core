//! A native model names each union arm `PascalCase(arm name)` (Rust/Kotlin variants, Swift cases), so two arms whose
//! names give one identifier cannot both be generated; an entity field's arms are keyed by name, so one is lost.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::schema::types::{FieldDefinition, FieldType, PayloadField};
use crate::str_utils::split_to_pascal_case;

/// Two arms of one union that name the same model arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnionArmCollision {
    pub first: String,
    pub second: String,
    pub identifier: String,
}

impl core::fmt::Display for UnionArmCollision {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "union arms `{}` and `{}` both name the arm `{}`; a union holds one arm of each kind (one list, one map, one object), so name the others as aliases",
            self.first, self.second, self.identifier
        )
    }
}

/// The first pair of arms, in declared order, whose names give one arm identifier.
pub fn union_arm_collision<'a>(arms: impl IntoIterator<Item = &'a str>) -> Option<UnionArmCollision> {
    let mut seen: Vec<(&str, String)> = Vec::new();
    for arm in arms {
        let identifier = split_to_pascal_case(arm);
        if let Some((first, _)) = seen.iter().find(|(_, id)| *id == identifier) {
            return Some(UnionArmCollision { first: first.to_string(), second: arm.to_string(), identifier });
        }
        seen.push((arm, identifier));
    }
    None
}

/// The first colliding union anywhere in a payload field's shape.
pub fn payload_union_arm_collision(field: &PayloadField) -> Option<UnionArmCollision> {
    let props = field.properties.as_deref().unwrap_or_default();
    if field.field_type == "union" {
        if let Some(found) = union_arm_collision(props.iter().map(|p| p.name.as_str())) {
            return Some(found);
        }
    }
    props
        .iter()
        .chain(field.items.as_deref())
        .chain(field.additional_properties.as_deref())
        .find_map(payload_union_arm_collision)
}

/// The first colliding union anywhere in an entity field's shape; its arm names are `values`, which keep every arm.
pub fn field_union_arm_collision(field: &FieldDefinition) -> Option<UnionArmCollision> {
    if field.field_type == FieldType::Union {
        if let Some(found) = union_arm_collision(field.values.iter().flatten().map(String::as_str)) {
            return Some(found);
        }
    }
    field
        .properties
        .iter()
        .flat_map(|props| props.values())
        .chain(field.items.as_deref())
        .chain(field.additional_properties.as_deref())
        .find_map(field_union_arm_collision)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(json: serde_json::Value) -> PayloadField {
        serde_json::from_value(json).expect("payload field")
    }

    fn field(json: serde_json::Value) -> FieldDefinition {
        serde_json::from_value(json).expect("field definition")
    }

    #[test]
    fn two_list_arms_collide() {
        let found = union_arm_collision(["array", "array"]).expect("collision");
        assert_eq!((found.first.as_str(), found.second.as_str(), found.identifier.as_str()), ("array", "array", "Array"));
    }

    #[test]
    fn arm_names_that_give_one_identifier_collide() {
        assert_eq!(union_arm_collision(["string", "my_type", "MyType"]).map(|c| c.identifier), Some("MyType".into()));
    }

    #[test]
    fn control_distinct_arms_pass() {
        assert_eq!(union_arm_collision(["string", "number", "array", "map", "object"]), None);
        assert_eq!(union_arm_collision(["string", "array"]), None);
    }

    #[test]
    fn a_payload_union_nested_in_a_list_is_checked() {
        let p = payload(serde_json::json!({
            "name": "rows", "type": "[Mixed]",
            "items": { "name": "", "type": "union", "properties": [
                { "name": "array", "type": "[string]" }, { "name": "array", "type": "[number]" } ] }
        }));
        assert_eq!(payload_union_arm_collision(&p).map(|c| c.identifier), Some("Array".into()));
    }

    #[test]
    fn control_a_payload_union_of_distinct_arms_passes() {
        let p = payload(serde_json::json!({ "name": "from", "type": "union", "properties": [
            { "name": "string", "type": "string" }, { "name": "array", "type": "[string]" } ] }));
        assert_eq!(payload_union_arm_collision(&p), None);
    }

    #[test]
    fn an_entity_union_whose_arms_were_keyed_away_is_still_seen_by_its_values() {
        let f = field(serde_json::json!({ "name": "value", "type": "union", "values": ["array", "array"],
            "properties": { "array": { "name": "array", "type": "array", "items": { "type": "number" } } } }));
        assert_eq!(field_union_arm_collision(&f).map(|c| c.identifier), Some("Array".into()));
    }

    #[test]
    fn control_an_entity_union_of_distinct_arms_passes() {
        let f = field(serde_json::json!({ "name": "other", "type": "union", "values": ["string", "array", "map"],
            "properties": { "string": { "name": "string", "type": "string" }, "array": { "name": "array", "type": "array", "items": { "type": "string" } }, "map": { "name": "map", "type": "object", "items": { "type": "string" } } } }));
        assert_eq!(field_union_arm_collision(&f), None);
    }
}
