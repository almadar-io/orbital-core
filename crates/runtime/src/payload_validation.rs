//! The API-boundary check of an event's payload against its trait's declared `payloadSchema`: required
//! fields, and the container (`[X]` array, `object`) a present field's type declares. JS twin:
//! `validateEventPayload` / `formatPayloadValidationError` (`almadar-runtime/src/traits/PayloadValidator.ts`).

use orbital_core_base::schema::types::PayloadField;
use orbital_core_base::value::Value;

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

enum Container {
    Array,
    Object,
    Opaque,
}

/// The container a payload field's `type` declares (`payloadTypeContainer`).
fn container_of(declared: &str) -> Container {
    if declared == "array"
        || (declared.len() > 2 && declared.starts_with('[') && declared.ends_with(']'))
    {
        Container::Array
    } else if declared == "object" {
        Container::Object
    } else {
        Container::Opaque
    }
}

/// One failure per violated field of `schema`; empty when the payload is valid or the event declares none.
pub fn validate_event_payload(
    event: &str,
    payload: &Value,
    schema: &[PayloadField],
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
            Some(Value::Null) => {
                if field.required == Some(true) {
                    failures.push(fail(PayloadFailureReason::Null));
                }
            }
            Some(value) => {
                let wrong = match container_of(&field.field_type) {
                    Container::Array => !matches!(value, Value::Array(_)),
                    Container::Object => !matches!(value, Value::Object(_)),
                    Container::Opaque => false,
                };
                if wrong {
                    failures.push(fail(PayloadFailureReason::WrongType));
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
    }

    #[test]
    fn edge_a_scalar_type_and_an_entity_name_are_opaque() {
        let schema = [field("n", "number", false), field("row", "Note", false)];
        assert!(
            validate_event_payload("E", &payload(json!({ "n": "x", "row": 3 })), &schema)
                .is_empty()
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
