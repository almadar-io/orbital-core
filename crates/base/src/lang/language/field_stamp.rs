//! The render-ui fields walk, and the entity field stamp built on it. A pattern declaring a
//! `@fieldsContract` has its `fields` / `columns` entries stamped from the entity definition: `"form"` gives
//! editable SchemaFields (`name`, `label`, `type`, `required`, `hint`, `values`, `relation`), `"display"` gives
//! read-only meta (`key`, `type`, `values`, `relation`). Authored keys always win.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use super::pattern_registry::get_fields_contract;
use crate::ir::{OirObject, OirPattern, OirValue};
use crate::schema::types::{EntityDefinition, FieldDefinition};
use crate::value::{Value, ValueMap};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum FieldStampError {
    #[error("entity field `{field}`: its {what} does not serialize to a name")]
    NotAName { field: String, what: &'static str },
    #[error("a render-ui tree held a lambda where the IR carries plain values")]
    LambdaInTree,
    #[error("a render-ui tree held a sealed value where the IR carries plain values")]
    SealedInTree,
}

/// Call `visit` with the declared `@fieldsContract` and the map of every node of a `render-ui` tree whose
/// pattern declares one; nodes nest through `children`.
pub fn walk_fields_nodes(node: &mut Value, visit: &mut impl FnMut(&'static str, &mut ValueMap)) {
    match node {
        Value::Array(items) => {
            for item in items {
                walk_fields_nodes(item, visit);
            }
        }
        Value::Object(map) => {
            let contract = map
                .get("type")
                .and_then(Value::as_str)
                .and_then(get_fields_contract);
            if let Some(contract) = contract {
                visit(contract, map);
            }
            if let Some(children) = map.get_mut("children") {
                walk_fields_nodes(children, visit);
            }
        }
        _ => {}
    }
}

struct FieldMeta {
    type_name: String,
    required: bool,
    description: Option<String>,
    values: Option<Vec<String>>,
    relation: Option<(String, Option<String>)>,
}

fn wire_name<T: Serialize>(
    field: &str,
    what: &'static str,
    value: &T,
) -> Result<String, FieldStampError> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => Ok(name),
        _ => Err(FieldStampError::NotAName {
            field: field.to_string(),
            what,
        }),
    }
}

impl FieldMeta {
    fn of(name: &str, field: &FieldDefinition) -> Result<Self, FieldStampError> {
        let relation = match &field.relation {
            Some(relation) => Some((
                relation.entity.clone(),
                relation
                    .cardinality
                    .as_ref()
                    .map(|c| wire_name(name, "cardinality", c))
                    .transpose()?,
            )),
            None => None,
        };
        Ok(Self {
            type_name: wire_name(name, "type", &field.field_type)?,
            required: field.required.unwrap_or(false),
            description: field.description.clone().filter(|d| !d.is_empty()),
            values: field.values.clone().filter(|v| !v.is_empty()),
            relation,
        })
    }

    fn values_value(&self) -> Option<Value> {
        self.values.as_ref().map(|values| {
            Value::array(
                values
                    .iter()
                    .map(|v| Value::string(v.as_str()))
                    .collect::<Vec<_>>(),
            )
        })
    }

    fn relation_value(&self) -> Option<Value> {
        self.relation.as_ref().map(|(entity, cardinality)| {
            let mut map = ValueMap::default();
            map.insert("entity".into(), Value::string(entity.as_str()));
            if let Some(cardinality) = cardinality {
                map.insert("cardinality".into(), Value::string(cardinality.as_str()));
            }
            Value::object(map)
        })
    }
}

/// The stamp source of one entity: its named fields' meta.
pub struct EntityFieldStamps {
    fields: HashMap<String, FieldMeta>,
}

impl EntityFieldStamps {
    pub fn of(entity: &EntityDefinition) -> Result<Self, FieldStampError> {
        let mut fields = HashMap::new();
        for field in &entity.fields {
            if let Some(name) = field.name.as_deref().filter(|n| !n.is_empty()) {
                fields.insert(name.to_string(), FieldMeta::of(name, field)?);
            }
        }
        Ok(Self { fields })
    }

    /// Stamp every fields-contract node of a `render-ui` tree.
    pub fn stamp_tree(&self, node: &mut Value) {
        walk_fields_nodes(node, &mut |contract, map| match contract {
            "form" => self.stamp_prop(map, "fields", &["name", "field"], Self::form_entry),
            "display" => {
                self.stamp_prop(map, "fields", &["key", "name"], Self::display_entry);
                self.stamp_prop(map, "columns", &["key", "name"], Self::display_entry);
            }
            _ => {}
        });
    }

    /// Stamp a resolved `render-ui` pattern in place.
    pub fn stamp_pattern(&self, pattern: &mut OirPattern) -> Result<(), FieldStampError> {
        let mut node = ValueMap::default();
        node.insert("type".into(), Value::string(pattern.pattern_type.as_str()));
        for (key, value) in &pattern.config {
            node.insert(key.as_str().into(), value_of(value));
        }
        let mut node = Value::object(node);
        self.stamp_tree(&mut node);
        let Value::Object(map) = node else {
            return Ok(());
        };
        let mut config = OirObject::default();
        for (key, value) in map.iter() {
            if &**key != "type" {
                config.insert(key.to_string(), oir_of(value)?);
            }
        }
        pattern.config = config;
        Ok(())
    }

    fn stamp_prop(
        &self,
        map: &mut ValueMap,
        prop: &str,
        name_keys: &[&str],
        entry: fn(&Self, &mut Value, &[&str]),
    ) {
        let Some(Value::Array(entries)) = map.get_mut(prop) else {
            return;
        };
        if entries.first().and_then(Value::as_str) == Some("fn") {
            return;
        }
        for item in entries.iter_mut() {
            entry(self, item, name_keys);
        }
    }

    fn form_entry(&self, entry: &mut Value, name_keys: &[&str]) {
        let stamped = match entry {
            Value::String(name) => self.fields.get(&***name).map(|meta| {
                let mut map = ValueMap::default();
                map.insert("name".into(), Value::string(name.clone()));
                map.insert("label".into(), Value::string(name.clone()));
                map.insert("type".into(), Value::string(meta.type_name.as_str()));
                map.insert("required".into(), Value::Bool(meta.required));
                if let Some(description) = &meta.description {
                    map.insert("hint".into(), Value::string(description.as_str()));
                }
                if let Some(values) = meta.values_value() {
                    map.insert("values".into(), values);
                }
                if let Some(relation) = meta.relation_value() {
                    map.insert("relation".into(), relation);
                }
                Value::object(map)
            }),
            Value::Object(map) => {
                if map.contains_key("type") || map.contains_key("inputType") {
                    return;
                }
                let Some(meta) = self.meta_named(map, name_keys) else {
                    return;
                };
                map.insert("type".into(), Value::string(meta.type_name.as_str()));
                if meta.required && !map.contains_key("required") {
                    map.insert("required".into(), Value::Bool(true));
                }
                if let Some(description) = &meta.description {
                    if !map.contains_key("hint") && !map.contains_key("help") {
                        map.insert("hint".into(), Value::string(description.as_str()));
                    }
                }
                if !map.contains_key("values") && !map.contains_key("options") {
                    if let Some(values) = meta.values_value() {
                        map.insert("values".into(), values);
                    }
                }
                if !map.contains_key("relation") {
                    if let Some(relation) = meta.relation_value() {
                        map.insert("relation".into(), relation);
                    }
                }
                None
            }
            _ => None,
        };
        if let Some(stamped) = stamped {
            *entry = stamped;
        }
    }

    fn display_entry(&self, entry: &mut Value, name_keys: &[&str]) {
        let stamped = match entry {
            Value::String(name) => self.fields.get(&***name).map(|meta| {
                let mut map = ValueMap::default();
                map.insert("key".into(), Value::string(name.clone()));
                map.insert("type".into(), Value::string(meta.type_name.as_str()));
                if let Some(values) = meta.values_value() {
                    map.insert("values".into(), values);
                }
                if let Some(relation) = meta.relation_value() {
                    map.insert("relation".into(), relation);
                }
                Value::object(map)
            }),
            Value::Object(map) => {
                if map.contains_key("type") {
                    return;
                }
                let Some(meta) = self.meta_named(map, name_keys) else {
                    return;
                };
                map.insert("type".into(), Value::string(meta.type_name.as_str()));
                if !map.contains_key("values") {
                    if let Some(values) = meta.values_value() {
                        map.insert("values".into(), values);
                    }
                }
                if !map.contains_key("relation") {
                    if let Some(relation) = meta.relation_value() {
                        map.insert("relation".into(), relation);
                    }
                }
                None
            }
            _ => None,
        };
        if let Some(stamped) = stamped {
            *entry = stamped;
        }
    }

    fn meta_named(&self, entry: &ValueMap, name_keys: &[&str]) -> Option<&FieldMeta> {
        let name = name_keys
            .iter()
            .find_map(|k| entry.get(*k).and_then(Value::as_str))?;
        self.fields.get(name)
    }
}

fn value_of(value: &OirValue) -> Value {
    match value {
        OirValue::Null => Value::Null,
        OirValue::Bool(b) => Value::Bool(*b),
        OirValue::Number(n) => Value::Number(*n),
        OirValue::String(s) => Value::string(s.as_str()),
        OirValue::Array(items) => Value::array(items.iter().map(value_of).collect::<Vec<_>>()),
        OirValue::Object(map) => {
            let mut out = ValueMap::default();
            for (key, value) in map {
                out.insert(key.as_str().into(), value_of(value));
            }
            Value::object(out)
        }
    }
}

fn oir_of(value: &Value) -> Result<OirValue, FieldStampError> {
    Ok(match value {
        Value::Null => OirValue::Null,
        Value::Bool(b) => OirValue::Bool(*b),
        Value::Number(n) => OirValue::Number(*n),
        Value::String(s) => OirValue::String(s.to_string()),
        Value::Array(items) => OirValue::Array(items.iter().map(oir_of).collect::<Result<_, _>>()?),
        Value::Object(map) => {
            let mut out = OirObject::default();
            for (key, value) in map.iter() {
                out.insert(key.to_string(), oir_of(value)?);
            }
            OirValue::Object(out)
        }
        Value::Lambda(_) => return Err(FieldStampError::LambdaInTree),
        Value::Sealed(_) => return Err(FieldStampError::SealedInTree),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity() -> EntityDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "Task",
            "fields": [
                { "name": "title", "type": "string", "required": true, "description": "What to do" },
                { "name": "status", "type": "string", "values": ["open", "done"] },
                { "name": "note", "type": "string" },
                { "name": "owner", "type": "relation", "relation": { "entity": "User", "cardinality": "many-to-one" } }
            ]
        }))
        .expect("entity parses")
    }

    fn tree(json: serde_json::Value) -> Value {
        serde_json::from_value(json).expect("tree parses")
    }

    fn stamped(json: serde_json::Value) -> serde_json::Value {
        let mut node = tree(json);
        EntityFieldStamps::of(&entity())
            .expect("stamps")
            .stamp_tree(&mut node);
        serde_json::to_value(&node).expect("serializes")
    }

    #[test]
    fn form_bare_name_is_stamped_from_the_entity() {
        let out =
            stamped(serde_json::json!({ "type": "form", "fields": ["title", "status", "owner"] }));
        assert_eq!(
            out["fields"],
            serde_json::json!([
                { "name": "title", "label": "title", "type": "string", "required": true, "hint": "What to do" },
                { "name": "status", "label": "status", "type": "string", "required": false, "values": ["open", "done"] },
                { "name": "owner", "label": "owner", "type": "relation", "required": false,
                  "relation": { "entity": "User", "cardinality": "many-to-one" } }
            ])
        );
    }

    #[test]
    fn form_object_with_a_type_is_untouched() {
        let entry = serde_json::json!({ "name": "title", "type": "textarea", "label": "Title" });
        let out = stamped(serde_json::json!({ "type": "form", "fields": [entry.clone()] }));
        assert_eq!(out["fields"][0], entry);
        let by_input = serde_json::json!({ "name": "title", "inputType": "text" });
        let out = stamped(serde_json::json!({ "type": "form", "fields": [by_input.clone()] }));
        assert_eq!(out["fields"][0], by_input);
    }

    #[test]
    fn form_partial_object_keeps_authored_keys_and_fills_the_rest() {
        let out = stamped(serde_json::json!({
            "type": "form",
            "fields": [
                { "name": "title", "label": "Heading", "help": "mine", "required": false },
                { "name": "status", "options": ["x"] }
            ]
        }));
        assert_eq!(
            out["fields"][0],
            serde_json::json!({ "name": "title", "label": "Heading", "help": "mine", "required": false, "type": "string" })
        );
        assert_eq!(
            out["fields"][1],
            serde_json::json!({ "name": "status", "options": ["x"], "type": "string" })
        );
    }

    #[test]
    fn unknown_names_and_lambda_fields_are_left_as_authored() {
        let out = stamped(
            serde_json::json!({ "type": "form", "fields": ["ghost", { "name": "ghost2" }] }),
        );
        assert_eq!(
            out["fields"],
            serde_json::json!(["ghost", { "name": "ghost2" }])
        );
        let lambda = serde_json::json!(["fn", ["f"], "title"]);
        let out = stamped(serde_json::json!({ "type": "form", "fields": lambda.clone() }));
        assert_eq!(out["fields"], lambda);
    }

    #[test]
    fn display_fields_and_columns_are_stamped_author_wins() {
        let out = stamped(serde_json::json!({
            "type": "detail-panel",
            "fields": ["status", { "key": "owner", "values": ["mine"] }, { "key": "note", "type": "html" }],
            "columns": ["title"]
        }));
        assert_eq!(
            out["fields"][0],
            serde_json::json!({ "key": "status", "type": "string", "values": ["open", "done"] })
        );
        assert_eq!(
            out["fields"][1],
            serde_json::json!({ "key": "owner", "values": ["mine"], "type": "relation",
                "relation": { "entity": "User", "cardinality": "many-to-one" } })
        );
        assert_eq!(
            out["fields"][2],
            serde_json::json!({ "key": "note", "type": "html" })
        );
        assert_eq!(
            out["columns"],
            serde_json::json!([{ "key": "title", "type": "string" }])
        );
    }

    #[test]
    fn children_nest_and_non_contract_patterns_are_untouched() {
        let out = stamped(serde_json::json!({
            "type": "stat-card",
            "fields": ["title"],
            "children": [{ "type": "form", "fields": ["note"] }]
        }));
        assert_eq!(out["fields"], serde_json::json!(["title"]));
        assert_eq!(out["children"][0]["fields"][0]["type"], "string");
    }
}
