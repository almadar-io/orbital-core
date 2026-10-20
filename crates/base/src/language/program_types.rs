//! The program value types of the language: what an `orbital`, `trait` or `behavior` value is as a record, read
//! from the installed `@almadar/core` (`orbLanguage.programTypes`, generated from its schemas), never baked in.
//!
//! The data is `.orb` field definitions by type name, so the one `field_shape` that types a program's own struct
//! aliases types these too.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::ir::types::{field_shape, OirType};
use crate::schema::types::FieldDefinition;

/// The language value types by name (`OrbitalValue`, `TraitValue`, `BehaviorRef`), from `text`.
pub fn parse(text: &str) -> Result<BTreeMap<String, OirType>, String> {
    Ok(definitions(text)?
        .into_iter()
        .map(|(name, field)| (name, field_shape(&field)))
        .collect())
}

/// The language value types' `.orb` field definitions by name, each checked to be a record.
fn definitions(text: &str) -> Result<BTreeMap<String, FieldDefinition>, String> {
    let declared: BTreeMap<String, serde_json::Value> =
        serde_json::from_str(text).map_err(|e| format!("invalid program types: {e}"))?;
    declared
        .into_iter()
        .map(|(name, value)| {
            let field: FieldDefinition = serde_json::from_value(value)
                .map_err(|e| format!("program type `{name}`: invalid field definition: {e}"))?;
            check(&field).map_err(|reason| format!("program type `{name}`: {reason}"))?;
            Ok((name, field))
        })
        .collect()
}

/// The members of the record `field`, each named, in name order.
fn members(field: FieldDefinition) -> Vec<FieldDefinition> {
    let mut members: Vec<FieldDefinition> = field
        .properties
        .into_iter()
        .flatten()
        .map(|(name, mut member)| {
            member.name = Some(name);
            member
        })
        .collect();
    members.sort_by(|a, b| a.name.cmp(&b.name));
    members
}

/// A program value type is a record or a tagged union of records, so a read through it has members to find.
fn check(field: &FieldDefinition) -> Result<(), String> {
    use crate::schema::types::FieldType;
    match field.field_type {
        FieldType::Object if field.properties.is_some() || field.items.is_some() => Ok(()),
        FieldType::Union if field.properties.is_some() => Ok(()),
        _ => Err("is not a record".to_string()),
    }
}

static TABLE: OnceLock<BTreeMap<String, OirType>> = OnceLock::new();

fn table() -> &'static BTreeMap<String, OirType> {
    TABLE.get_or_init(|| {
        let data = crate::language_data::language_data_after_preflight();
        parse(&data.program_types).unwrap_or_default()
    })
}

/// The installed language's type of `name`, when it declares one.
pub fn program_type(name: &str) -> Option<&'static OirType> {
    table().get(name)
}

static FIELDS: OnceLock<BTreeMap<String, FieldDefinition>> = OnceLock::new();

/// The installed language's `.orb` field definition of the program type `name`, when it declares one.
pub fn program_type_field(name: &str) -> Option<&'static FieldDefinition> {
    FIELDS
        .get_or_init(|| {
            let data = crate::language_data::language_data_after_preflight();
            definitions(&data.program_types).unwrap_or_default()
        })
        .get(name)
}

static VIEWER: OnceLock<Vec<FieldDefinition>> = OnceLock::new();

/// The installed language's `Viewer` members (who `@user` is), each named, in name order.
pub fn viewer_fields() -> &'static [FieldDefinition] {
    VIEWER.get_or_init(|| {
        program_type_field("Viewer")
            .cloned()
            .map(members)
            .unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text() -> String {
        json!({
            "Sample": {
                "type": "object",
                "properties": {
                    "behavior": { "type": "string", "required": true },
                    "pages": { "type": "object", "items": { "type": "string" } },
                    "data": { "type": "SExpr" },
                    "source": {
                        "type": "union",
                        "values": ["a", "b"],
                        "properties": {
                            "a": { "type": "object", "properties": { "kind": { "type": "enum", "values": ["a"], "required": true } } },
                            "b": { "type": "object", "properties": { "kind": { "type": "enum", "values": ["b"], "required": true } } }
                        }
                    }
                }
            }
        })
        .to_string()
    }

    #[test]
    fn a_record_is_its_members_typed() {
        let table = parse(&text()).unwrap();
        let OirType::Struct { fields } = &table["Sample"] else {
            panic!("a record");
        };
        let by_name = |n: &str| {
            fields
                .iter()
                .find(|f| f.name == n)
                .unwrap_or_else(|| panic!("no member {n}"))
        };
        assert_eq!(by_name("behavior").field_type, OirType::String);
        assert!(by_name("behavior").required);
        assert!(!by_name("pages").required);
        assert_eq!(
            by_name("pages").field_type,
            OirType::map(OirType::String, OirType::String)
        );
        assert_eq!(by_name("data").field_type, OirType::SExpr);
        assert!(
            matches!(&by_name("source").field_type, OirType::Union { variants } if variants.len() == 2)
        );
    }

    #[test]
    fn a_records_members_are_named_and_in_name_order() {
        let mut d = definitions(&text()).unwrap();
        let names: Vec<Option<String>> = members(d.remove("Sample").unwrap())
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert_eq!(
            names,
            ["behavior", "data", "pages", "source"].map(|n| Some(n.to_string()))
        );
        let map: FieldDefinition =
            serde_json::from_value(json!({ "type": "object", "items": { "type": "string" } })).unwrap();
        assert!(members(map).is_empty(), "control: a map record declares no members");
    }

    #[test]
    fn what_is_not_a_record_or_not_a_type_is_refused() {
        let err = parse(r#"{"X":{"type":"string"}}"#).unwrap_err();
        assert!(err.contains("`X`") && err.contains("not a record"), "{err}");
        assert!(parse(r#"{"X":{"type":"nonsense"}}"#)
            .unwrap_err()
            .contains("program type `X`"));
        assert!(parse("[]").unwrap_err().contains("invalid program types"));
        assert!(
            parse("{}").unwrap().is_empty(),
            "control: no types is a table with none"
        );
    }
}
