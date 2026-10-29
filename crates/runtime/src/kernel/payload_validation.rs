//! API-boundary event payload validation through the kernel's declared-shape checker.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use orbital_core_base::ir::types::{payload_field_shape, DeclaredShape, OirType};
use orbital_core_base::schema::types::PayloadField;
use orbital_core_base::value::Value;
use orbital_core_eval::evaluator::operators::arg_types::misfit_detail_of_shape;
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
    validate_event_payload_with_context(
        event,
        payload,
        schema,
        &|name| named.get(name).cloned(),
    )
}

pub(crate) fn validate_event_payload_with_context(
    event: &str,
    payload: &Value,
    schema: &[PayloadField],
    resolve: &dyn Fn(&str) -> Option<OirType>,
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
            Some(value) => {
                let declared =
                    DeclaredShape::with_resolver(None, payload_field_shape(field), resolve);
                if let Some(found) = misfit_detail_of_shape(value, &declared) {
                    let mut failure = fail(if found.actual == "absent" {
                        PayloadFailureReason::Missing
                    } else if found.actual == "null" {
                        PayloadFailureReason::Null
                    } else {
                        PayloadFailureReason::WrongType
                    });
                    failure.field.push_str(&found.path);
                    failure.expected_type = found.expected;
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
    fn a_member_without_bang_may_be_absent_but_null_needs_a_nullable_declaration() {
        let schema: Vec<PayloadField> = serde_json::from_value(json!([{
            "name": "viewer", "type": "object", "required": true, "properties": [
                {"name": "id", "type": "string", "required": true},
                {"name": "role", "type": "string", "required": false},
                {"name": "permissions", "type": "[string]", "required": false, "nullable": true}
            ]
        }, {"name": "note", "type": "string", "required": false},
           {"name": "tag", "type": "string", "required": false, "nullable": true}]))
        .unwrap();
        let versioned = |value| {
            validate_event_payload_with_context("E", &payload(value), &schema, &|_| None)
        };
        for ok in [
            json!({"viewer": {"id": "u"}}),
            json!({"viewer": {"id": "u", "permissions": null}, "tag": null}),
        ] {
            assert!(versioned(ok).is_empty());
        }
        for (bad, field) in [
            (json!({"viewer": {"id": "u", "role": null}}), "viewer.role"),
            (json!({"viewer": {"id": null}}), "viewer.id"),
            (json!({"viewer": {"id": "u"}, "note": null}), "note"),
        ] {
            let failures = versioned(bad);
            assert_eq!(failures.len(), 1, "{field}");
            assert_eq!(failures[0].field, field);
        }
    }

    #[test]
    fn a_misfit_names_the_type_declared_at_its_own_path() {
        let schema: Vec<PayloadField> = serde_json::from_value(json!([
            {"name": "units", "type": "[object]", "properties": [
                {"name": "content", "type": "string", "required": true}
            ]},
            {"name": "counts", "type": "object", "items": {"name": "", "type": "number"}},
            {"name": "total", "type": "number"}
        ]))
        .unwrap();
        let failures = validate_event_payload(
            "E",
            &payload(json!({
                "units": [{"content": 4}],
                "counts": {"one": "bad"},
                "total": "x"
            })),
            &schema,
        );
        let named: Vec<(&str, &str)> = failures
            .iter()
            .map(|f| (f.field.as_str(), f.expected_type.as_str()))
            .collect();
        assert_eq!(
            named,
            [
                ("units[0].content", "string"),
                ("counts.one", "number"),
                ("total", "number")
            ]
        );
        let absent = validate_event_payload("E", &payload(json!({"units": [{}]})), &schema);
        assert_eq!(
            (absent[0].field.as_str(), absent[0].expected_type.as_str()),
            ("units[0].content", "string")
        );
        assert!(validate_event_payload(
            "E",
            &payload(json!({"units": [{"content": "ok"}], "counts": {"one": 1}, "total": 2})),
            &schema
        )
        .is_empty());
    }

    #[test]
    fn an_instant_member_takes_zoned_iso_text_and_refuses_a_number() {
        let schema = [field("n", "instant", true)];
        let check = |value: serde_json::Value| validate_event_payload("GO", &payload(json!({ "n": value })), &schema);
        assert!(check(json!("2026-10-29T02:30:00.000Z")).is_empty());
        assert!(check(json!("2026-10-29T02:30:00+02:00")).is_empty());
        assert_eq!(check(json!(1_700_000_000_000_i64)).len(), 1);
        assert_eq!(check(json!("2026-10-29")).len(), 1);
        assert_eq!(check(json!("2026-10-29T02:30:00")).len(), 1);
        assert_eq!(format_payload_failures(&check(json!(0))), "Payload validation failed for event 'GO': n (wrong-type, expected instant)");
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
