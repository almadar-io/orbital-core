//! API-boundary event payload validation through the kernel's declared-shape checker.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::ir::types::{payload_field_shape, DeclaredShape, OirType};
use orbital_core_base::schema::types::PayloadField;
use orbital_core_base::value::Value;
use orbital_core_eval::evaluator::operators::arg_types::misfit_of_shape;
use std::collections::BTreeMap;

/// One violation of an event's declared payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadFailure {
    pub event: String,
    pub field: String,
    pub reason: PayloadFailureReason,
    pub expected_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadFailureReason {
    Missing,
    Null,
    WrongType,
}

impl PayloadFailureReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Null => "null",
            Self::WrongType => "wrong-type",
        }
    }
}

/// One failure per violated field of `schema`; empty when the payload is valid or the event declares none.
pub fn validate_event_payload(
    event: &str,
    payload: &Value,
    schema: &[PayloadField],
) -> Vec<PayloadFailure> {
    validate_event_payload_with_types(event, payload, schema, &BTreeMap::new())
}

pub fn validate_event_payload_with_types(
    event: &str,
    payload: &Value,
    schema: &[PayloadField],
    named: &BTreeMap<String, OirType>,
) -> Vec<PayloadFailure> {
    validate_event_payload_with_context(event, payload, schema, &|name| named.get(name).cloned(), None)
}

pub(crate) fn validate_event_payload_with_context(
    event: &str,
    payload: &Value,
    schema: &[PayloadField],
    resolve: &dyn Fn(&str) -> Option<OirType>,
    version: Option<u32>,
) -> Vec<PayloadFailure> {
    let mut failures = Vec::new();
    for field in schema {
        let fail = |reason| PayloadFailure {
            event: event.to_string(),
            field: field.name.clone(),
            reason,
            expected_type: field.field_type.clone(),
        };
        match payload
            .as_object()
            .and_then(|fields| fields.get(&field.name))
        {
            None => {
                if field.required == Some(true) {
                    failures.push(fail(PayloadFailureReason::Missing));
                }
            }
            Some(Value::Null) if version.is_none() => {
                if field.required == Some(true) {
                    failures.push(fail(PayloadFailureReason::Null));
                }
            }
            Some(value) => {
                let mut declared =
                    DeclaredShape::with_resolver(None, payload_field_shape(field), resolve);
                declared.version = version;
                if let Some((path, actual)) = misfit_of_shape(value, &declared) {
                    let mut failure = fail(if actual == "absent" {
                        PayloadFailureReason::Missing
                    } else if version == Some(1) && actual == "null" {
                        PayloadFailureReason::Null
                    } else {
                        PayloadFailureReason::WrongType
                    });
                    failure.field.push_str(&path);
                    failures.push(failure);
                }
            }
        }
    }
    failures
}

/// The API response's error text for `failures` (empty for none).
pub fn format_payload_failures(failures: &[PayloadFailure]) -> String {
    let Some(first) = failures.first() else {
        return String::new();
    };
    let parts: Vec<String> = failures
        .iter()
        .map(|f| {
            let expected = if f.expected_type.is_empty() {
                String::new()
            } else {
                format!(", expected {}", f.expected_type)
            };
            format!("{} ({}{})", f.field, f.reason.as_str(), expected)
        })
        .collect();
    format!(
        "Payload validation failed for event '{}': {}",
        first.event,
        parts.join("; ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field(name: &str, ty: &str, required: bool) -> PayloadField {
        serde_json::from_value(json!({ "name": name, "type": ty, "required": required })).unwrap()
    }

    fn payload(value: serde_json::Value) -> Value {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_missing_required_field_is_reported_and_an_optional_one_is_not() {
        let schema = [field("a", "string", true), field("b", "string", false)];
        let failures = validate_event_payload("E", &payload(json!({})), &schema);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].field, "a");
        assert_eq!(failures[0].reason, PayloadFailureReason::Missing);
    }

    #[test]
    fn the_bare_array_primitive_and_the_object_type_are_containers_too() {
        let schema = [
            field("list", "array", false),
            field("data", "object", false),
        ];
        assert!(
            validate_event_payload("E", &payload(json!({ "list": [1], "data": {} })), &schema)
                .is_empty()
        );
        let bad =
            validate_event_payload("E", &payload(json!({ "list": "x", "data": [1] })), &schema);
        assert_eq!(bad.len(), 2);
        let objects = [field("rows", "[object]", false)];
        assert_eq!(
            validate_event_payload("E", &payload(json!({"rows": [1]})), &objects)[0].field,
            "rows[0]"
        );
    }

    #[test]
    fn scalar_types_are_checked_and_unresolved_entity_references_accept_ids() {
        let schema = [field("n", "number", false), field("row", "Note", false)];
        assert!(validate_event_payload(
            "E",
            &payload(json!({ "n": 1, "row": "note-id" })),
            &schema
        )
        .is_empty());
    }

    #[test]
    fn scalar_maps_and_closed_literals_use_the_declared_shape_checker() {
        let schema: Vec<PayloadField> = serde_json::from_value(json!([
            {"name": "count", "type": "number"},
            {"name": "mode", "type": "string", "values": ["on", "off"]},
            {"name": "counts", "type": "object", "items": {"name": "", "type": "number"}}
        ]))
        .unwrap();
        let failures = validate_event_payload(
            "E",
            &payload(json!({"count": "bad", "mode": "other", "counts": {"one": "bad"}})),
            &schema,
        );
        assert_eq!(
            failures
                .iter()
                .map(|failure| failure.field.as_str())
                .collect::<Vec<_>>(),
            ["count", "mode", "counts.one"]
        );
        assert!(validate_event_payload(
            "E",
            &payload(json!({"count": 1, "mode": "on", "counts": {"one": 1}})),
            &schema
        )
        .is_empty());
    }

    #[test]
    fn nested_array_records_reject_a_misfit_at_its_declared_path() {
        let schema: Vec<PayloadField> = serde_json::from_value(json!([{
            "name": "rows", "type": "[object]", "properties": [
                {"name": "count", "type": "number", "required": true}
            ]
        }]))
        .unwrap();
        let failures = validate_event_payload(
            "E",
            &payload(json!({"rows": [{"count": "wrong"}]})),
            &schema,
        );
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].field, "rows[0].count");
        assert!(
            validate_event_payload("E", &payload(json!({"rows": [{"count": 2}]})), &schema)
                .is_empty()
        );
        assert_eq!(
            validate_event_payload("E", &payload(json!({"rows": [{}]})), &schema)[0].reason,
            PayloadFailureReason::Missing
        );
    }

    #[test]
    fn the_error_text_lists_every_failure_under_the_first_event() {
        let schema = [field("a", "string", true), field("b", "[x]", false)];
        let failures = validate_event_payload("E", &payload(json!({ "b": 1 })), &schema);
        assert_eq!(
            format_payload_failures(&failures),
            "Payload validation failed for event 'E': a (missing, expected string); b (wrong-type, expected [x])"
        );
        assert_eq!(format_payload_failures(&[]), "");
    }
}
