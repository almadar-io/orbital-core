//! A declared payload as the JSON Schema a tool-calling model fills, and a value checked against it; and the
//! declared input a `listens`-addressed event names. The meaning of a payload declaration is the kernel's: hosts ask
//! (`contractQuery`) and carry none of these rules (EXEC-BOUND).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::external_inputs::ExternalInput;
use crate::schema::types::{split_event_address, ListenSource, PayloadField};
use serde::Serialize;
use serde_json::{Map, Value};

/// The declared input a `listens`-addressed event names, resolved from the calling trait's position: `EVENT` is that
/// trait's own, `Trait.EVENT` a trait of its orbital, `Orbital.Trait.EVENT` any orbital. A `*.EVENT` wildcard names
/// none. Mirrors the L2 rule in `orbital-compiler` `config_type.rs` `EventRefIndex::check`.
pub fn find_input_by_address<'a>(
    inputs: &'a [ExternalInput],
    address: &str,
    caller_orbital: &str,
    caller_trait: &str,
) -> Option<&'a ExternalInput> {
    let (event, source) = split_event_address(address);
    let (orbital, trait_name) = match &source {
        Some(ListenSource::Any) => return None,
        Some(ListenSource::Orbital {
            orbital,
            trait_name,
            ..
        }) => (orbital.as_str(), trait_name.as_str()),
        Some(ListenSource::Trait { trait_name, .. }) => (caller_orbital, trait_name.as_str()),
        None => (caller_orbital, caller_trait),
    };
    inputs
        .iter()
        .find(|i| i.orbital == orbital && i.trait_name == trait_name && i.event == event)
}

fn json_type(declared: &str) -> Option<&'static str> {
    match declared {
        "string" | "date" | "timestamp" | "datetime" | "email" | "url" | "phone" | "uuid"
        | "image" | "bytes" | "enum" | "trait" | "slot" | "pattern" | "secret" | "event"
        | "EventAddress" | "CallKey" | "field" => Some("string"),
        "number" | "instant" => Some("number"),
        "boolean" => Some("boolean"),
        "array" => Some("array"),
        "object" | "relation" | "orbital" | "behavior" => Some("object"),
        _ => None,
    }
}

fn element_of(declared: &str) -> (bool, &str) {
    match declared
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        Some(element) => (true, element),
        None => (false, declared),
    }
}

/// A declared payload as the JSON Schema of one object: what a tool-calling model fills.
pub fn payload_json_schema(fields: &[PayloadField]) -> Value {
    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();
    for field in fields {
        if field.properties.as_ref().is_some_and(Vec::is_empty) && field.field_type != "union" {
            continue;
        }
        properties.insert(field.name.clone(), field_json_schema(field));
        if field.required == Some(true) {
            required.push(Value::String(field.name.clone()));
        }
    }
    let mut out = Map::new();
    out.insert("type".into(), Value::String("object".into()));
    if !fields.is_empty() {
        out.insert("additionalProperties".into(), Value::Bool(false));
    }
    out.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        out.insert("required".into(), Value::Array(required));
    }
    Value::Object(out)
}

fn field_json_schema(field: &PayloadField) -> Value {
    let (is_array, element) = element_of(&field.field_type);
    let mut shape = match &field.properties {
        Some(properties) => payload_json_schema(properties),
        None => {
            let mut base = Map::new();
            if let Some(t) = json_type(element) {
                base.insert("type".into(), Value::String(t.into()));
            }
            Value::Object(base)
        }
    };
    if let (Some(values), Value::Object(map)) = (&field.values, &mut shape) {
        map.insert(
            "enum".into(),
            Value::Array(values.iter().cloned().map(Value::String).collect()),
        );
    }
    if !is_array {
        return shape;
    }
    let mut array = Map::new();
    array.insert("type".into(), Value::String("array".into()));
    array.insert("items".into(), shape);
    Value::Object(array)
}

/// How a value misses its declared payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PayloadIssueReason {
    Missing,
    Unknown,
    WrongType,
    NotOneOf,
}

/// One way a value misses its declared payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PayloadIssue {
    pub path: String,
    pub reason: PayloadIssueReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
}

fn issue(path: &str, reason: PayloadIssueReason, expected: Option<String>) -> PayloadIssue {
    PayloadIssue {
        path: path.to_string(),
        reason,
        expected,
    }
}

/// Check a value against a declared payload: required fields, primitive and container types, closed value sets,
/// declared object shapes (closed) at any depth. An object field without declared properties stays open; a type with
/// no JSON shape (`node`, an entity name) is not checked beyond its container.
pub fn payload_issues(fields: &[PayloadField], value: &Value) -> Vec<PayloadIssue> {
    object_issues(fields, value, "")
}

fn join(at: &str, key: &str) -> String {
    if at.is_empty() {
        key.to_string()
    } else {
        format!("{at}.{key}")
    }
}

fn object_issues(fields: &[PayloadField], value: &Value, at: &str) -> Vec<PayloadIssue> {
    let Value::Object(map) = value else {
        let path = if at.is_empty() { "." } else { at };
        return vec![issue(
            path,
            PayloadIssueReason::WrongType,
            Some("object".into()),
        )];
    };
    let mut issues = Vec::new();
    for field in fields {
        match map.get(&field.name) {
            None | Some(Value::Null) => {
                if field.required == Some(true) {
                    issues.push(issue(
                        &join(at, &field.name),
                        PayloadIssueReason::Missing,
                        None,
                    ));
                }
            }
            Some(v) => issues.extend(field_issues(field, v, &join(at, &field.name))),
        }
    }
    let allowed = if fields.is_empty() {
        "(no keys)".to_string()
    } else {
        fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    };
    for key in map.keys() {
        if !fields.iter().any(|f| &f.name == key) {
            issues.push(issue(
                &join(at, key),
                PayloadIssueReason::Unknown,
                Some(allowed.clone()),
            ));
        }
    }
    issues
}

fn field_issues(field: &PayloadField, value: &Value, at: &str) -> Vec<PayloadIssue> {
    let (is_array, element) = element_of(&field.field_type);
    if !is_array && field.field_type != "array" {
        return element_issues(field, element, value, at);
    }
    let Value::Array(items) = value else {
        return vec![issue(
            at,
            PayloadIssueReason::WrongType,
            Some(field.field_type.clone()),
        )];
    };
    if !is_array {
        return Vec::new();
    }
    items
        .iter()
        .enumerate()
        .flat_map(|(i, item)| element_issues(field, element, item, &format!("{at}[{i}]")))
        .collect()
}

fn element_issues(
    field: &PayloadField,
    element: &str,
    value: &Value,
    at: &str,
) -> Vec<PayloadIssue> {
    if let Some(properties) = &field.properties {
        if field.field_type != "union" {
            return object_issues(properties, value, at);
        }
        let fits = properties
            .iter()
            .any(|v| element_issues(v, element_of(&v.field_type).1, value, at).is_empty());
        return if fits {
            Vec::new()
        } else {
            vec![issue(
                at,
                PayloadIssueReason::WrongType,
                Some(
                    properties
                        .iter()
                        .map(|v| v.field_type.as_str())
                        .collect::<Vec<_>>()
                        .join(" | "),
                ),
            )]
        };
    }
    let matches = match json_type(element) {
        None => true,
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("string") => value.is_string(),
        Some("number") => value.is_number(),
        Some("boolean") => value.is_boolean(),
        Some(_) => false,
    };
    if !matches {
        return vec![issue(
            at,
            PayloadIssueReason::WrongType,
            Some(element.to_string()),
        )];
    }
    if let Some(values) = &field.values {
        if !value
            .as_str()
            .is_some_and(|s| values.iter().any(|v| v == s))
        {
            return vec![issue(
                at,
                PayloadIssueReason::NotOneOf,
                Some(values.join(" | ")),
            )];
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fields(v: Value) -> Vec<PayloadField> {
        serde_json::from_value(v).expect("fields")
    }

    fn fill() -> Vec<PayloadField> {
        fields(json!([
            { "name": "layout", "type": "string", "required": true, "values": ["grid", "rows"] },
            { "name": "count", "type": "number" },
            { "name": "tags", "type": "[string]", "values": ["new", "sale"] },
            { "name": "config", "type": "object", "properties": [{ "name": "dense", "type": "boolean" }] },
            { "name": "rows", "type": "[object]", "properties": [{ "name": "id", "type": "string", "required": true }] }
        ]))
    }

    fn issues(value: Value) -> Value {
        serde_json::to_value(payload_issues(&fill(), &value)).unwrap()
    }

    #[test]
    fn a_matching_value_has_no_issues() {
        assert_eq!(
            issues(
                json!({ "layout": "grid", "count": 3, "tags": ["sale"], "config": { "dense": true }, "rows": [{ "id": "a" }] })
            ),
            json!([])
        );
    }

    #[test]
    fn a_missing_required_field_is_missing_and_null_counts_as_absent() {
        assert_eq!(
            issues(json!({})),
            json!([{ "path": "layout", "reason": "missing" }])
        );
        assert_eq!(
            issues(json!({ "layout": null })),
            json!([{ "path": "layout", "reason": "missing" }])
        );
    }

    #[test]
    fn a_value_outside_a_closed_set_is_refused_also_inside_an_array() {
        assert_eq!(
            issues(json!({ "layout": "cards", "tags": ["new", "old"] })),
            json!([
                { "path": "layout", "reason": "not-one-of", "expected": "grid | rows" },
                { "path": "tags[1]", "reason": "not-one-of", "expected": "new | sale" }
            ])
        );
    }

    #[test]
    fn a_wrong_primitive_container_and_nested_miss_are_named() {
        assert_eq!(
            issues(json!({ "layout": "grid", "count": "three", "config": [], "rows": [{}] })),
            json!([
                { "path": "count", "reason": "wrong-type", "expected": "number" },
                { "path": "config", "reason": "wrong-type", "expected": "object" },
                { "path": "rows[0].id", "reason": "missing" }
            ])
        );
        assert_eq!(
            serde_json::to_value(payload_issues(&fill(), &json!(3))).unwrap(),
            json!([{ "path": ".", "reason": "wrong-type", "expected": "object" }])
        );
    }

    #[test]
    fn a_key_the_declaration_does_not_name_is_unknown_at_any_depth() {
        assert_eq!(
            issues(
                json!({ "layout": "grid", "colour": "red", "config": { "dense": true, "wide": true } })
            ),
            json!([
                { "path": "config.wide", "reason": "unknown", "expected": "dense" },
                { "path": "colour", "reason": "unknown", "expected": "layout | count | tags | config | rows" }
            ])
        );
    }

    #[test]
    fn an_object_that_declares_no_keys_names_no_alternatives_and_an_undeclared_one_stays_open() {
        let roles = fields(json!([{ "name": "roles", "type": "object", "properties": [] }]));
        assert_eq!(
            serde_json::to_value(payload_issues(
                &roles,
                &json!({ "roles": { "Member": "StudioUser" } })
            ))
            .unwrap(),
            json!([{ "path": "roles.Member", "reason": "unknown", "expected": "(no keys)" }])
        );
        let open = fields(json!([{ "name": "data", "type": "object" }]));
        assert!(payload_issues(&open, &json!({ "data": { "anything": 1 } })).is_empty());
    }

    #[test]
    fn a_bare_array_field_checks_its_container_only_and_a_union_takes_any_variant() {
        let loose = fields(json!([{ "name": "xs", "type": "array" }]));
        assert!(payload_issues(&loose, &json!({ "xs": ["a", 1] })).is_empty());
        assert_eq!(
            serde_json::to_value(payload_issues(&loose, &json!({ "xs": "a" }))).unwrap(),
            json!([{ "path": "xs", "reason": "wrong-type", "expected": "array" }])
        );
        let union = fields(
            json!([{ "name": "v", "type": "union", "properties": [{ "name": "", "type": "string" }, { "name": "", "type": "number" }] }]),
        );
        assert!(payload_issues(&union, &json!({ "v": 3 })).is_empty());
        assert_eq!(
            serde_json::to_value(payload_issues(&union, &json!({ "v": true }))).unwrap(),
            json!([{ "path": "v", "reason": "wrong-type", "expected": "string | number" }])
        );
    }

    #[test]
    fn a_type_with_no_json_shape_is_not_checked() {
        let node = fields(json!([{ "name": "body", "type": "node" }]));
        assert!(payload_issues(&node, &json!({ "body": { "type": "stack" } })).is_empty());
        assert!(payload_issues(&node, &json!({ "body": "text" })).is_empty());
    }

    #[test]
    fn the_json_schema_carries_values_as_enum_element_wise_and_closes_declared_objects() {
        let f = fill();
        let picked = vec![f[0].clone(), f[2].clone(), f[3].clone()];
        assert_eq!(
            payload_json_schema(&picked),
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "layout": { "type": "string", "enum": ["grid", "rows"] },
                    "tags": { "type": "array", "items": { "type": "string", "enum": ["new", "sale"] } },
                    "config": { "type": "object", "additionalProperties": false, "properties": { "dense": { "type": "boolean" } } }
                },
                "required": ["layout"]
            })
        );
    }

    #[test]
    fn the_json_schema_maps_primitives_nested_objects_and_arrays_of_objects() {
        let schema = payload_json_schema(&fields(json!([
            { "name": "title", "type": "string", "required": true },
            { "name": "data", "type": "object", "required": true, "properties": [{ "name": "done", "type": "boolean", "required": true }] },
            { "name": "rows", "type": "[object]", "properties": [{ "name": "id", "type": "string", "required": true }] }
        ])));
        assert_eq!(
            schema,
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "title": { "type": "string" },
                    "data": { "type": "object", "additionalProperties": false, "properties": { "done": { "type": "boolean" } }, "required": ["done"] },
                    "rows": { "type": "array", "items": { "type": "object", "additionalProperties": false, "properties": { "id": { "type": "string" } }, "required": ["id"] } }
                },
                "required": ["title", "data"]
            })
        );
    }

    #[test]
    fn the_json_schema_leaves_out_an_object_that_declares_no_keys_but_offers_an_open_one() {
        let schema = payload_json_schema(&fields(json!([
            { "name": "roles", "type": "object", "properties": [] },
            { "name": "count", "type": "number" }
        ])));
        assert_eq!(
            schema,
            json!({ "type": "object", "additionalProperties": false, "properties": { "count": { "type": "number" } } })
        );
        assert!(
            payload_json_schema(&fields(json!([{ "name": "data", "type": "object" }])))
                ["properties"]
                .get("data")
                .is_some()
        );
        assert_eq!(
            payload_json_schema(&[]),
            json!({ "type": "object", "properties": {} })
        );
    }

    #[test]
    fn a_list_element_has_no_name_and_decodes_as_declared() {
        let via = fields(
            json!([{ "name": "via", "type": "array", "items": { "name": "", "type": "union" } }]),
        );
        assert_eq!(
            via[0]
                .items
                .as_ref()
                .map(|i| (i.name.as_str(), i.field_type.as_str())),
            Some(("", "union"))
        );
        let bare =
            fields(json!([{ "name": "via", "type": "array", "items": { "type": "union" } }]));
        assert_eq!(bare[0].items.as_ref().map(|i| i.name.as_str()), Some(""));
    }

    fn input(orbital: &str, trait_name: &str, event: &str) -> ExternalInput {
        ExternalInput {
            orbital: orbital.into(),
            trait_name: trait_name.into(),
            event: event.into(),
            payload_schema: Vec::new(),
            description: None,
            synonyms: None,
            tier: None,
        }
    }

    #[test]
    fn an_address_resolves_from_the_callers_position_and_only_to_a_declared_input() {
        let inputs = vec![
            input("TaskOrbital", "TaskPersistor", "DO_CREATE"),
            input("LeadOrbital", "LeadOrbitalDealPersistor", "DO_UPDATE"),
            input("TaskOrbital", "Assistant", "ASK"),
        ];
        let find = |address: &str| {
            find_input_by_address(&inputs, address, "TaskOrbital", "Assistant")
                .map(|i| i.event.as_str())
        };
        assert_eq!(find("TaskPersistor.DO_CREATE"), Some("DO_CREATE"));
        assert_eq!(
            find("LeadOrbital.LeadOrbitalDealPersistor.DO_UPDATE"),
            Some("DO_UPDATE")
        );
        assert_eq!(find("ASK"), Some("ASK"));
        assert_eq!(
            find("LeadOrbitalDealPersistor.DO_UPDATE"),
            None,
            "a trait address does not reach a sibling orbital"
        );
        assert_eq!(
            find("TaskPersistor.OPEN"),
            None,
            "control: not a declared input"
        );
        assert_eq!(find("*.DO_CREATE"), None, "a wildcard is not an address");
    }
}
