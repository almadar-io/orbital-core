//! The one mock `call-service` answer: twin of `mockValueForDescriptor` /
//! `generateMockFromShape` in `@almadar/integrations`' `RuntimeIntegrationManager`.
//!
//! An action's answer is built from the `responseShape` the installed services
//! registry declares for it, so every host (playground mock mode, `orb verify`
//! fixtures, native previews, `orbital-integrations` mock mode) answers a service
//! call with the same value. No randomness and no clock: the value is a function of
//! the registry shape and, for a `shapedBy` field, of the request param it names.
//! Parity with the TS function is pinned by `tests/mock_answer_parity.rs`.

use orbital_core_base::language_data::language_data;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub enum MockAnswerError {
    /// The installed language data could not be loaded.
    LanguageData(String),
    /// The services registry JSON could not be read.
    Registry(String),
    UnknownService(String),
    UnknownAction {
        service: String,
        action: String,
    },
    /// A `responseShape` field the generator has no rule for (a descriptor without a string `type`, a non-object `properties`).
    MalformedShape {
        service: String,
        action: String,
        field: String,
    },
}

impl fmt::Display for MockAnswerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LanguageData(e) => write!(f, "mock answer: language data unavailable: {e}"),
            Self::Registry(e) => write!(f, "mock answer: services registry unreadable: {e}"),
            Self::UnknownService(s) => write!(f, "mock answer: service '{s}' is not in the services registry"),
            Self::UnknownAction { service, action } => {
                write!(f, "No mock response for action: {service}.{action}")
            }
            Self::MalformedShape {
                service,
                action,
                field,
            } => write!(
                f,
                "mock answer: {service}.{action}: responseShape field '{field}' is not a shape descriptor"
            ),
        }
    }
}

impl std::error::Error for MockAnswerError {}

type Shapes = HashMap<String, HashMap<String, Map<String, Value>>>;

/// Service -> action -> `responseShape` (empty when the action declares none), parsed once.
fn shapes() -> Result<&'static Shapes, MockAnswerError> {
    static SHAPES: OnceLock<Result<Shapes, MockAnswerError>> = OnceLock::new();
    SHAPES
        .get_or_init(|| {
            let data = language_data().map_err(|e| MockAnswerError::LanguageData(e.to_string()))?;
            let root: Value = serde_json::from_str(&data.services)
                .map_err(|e| MockAnswerError::Registry(e.to_string()))?;
            let services = root
                .get("services")
                .and_then(Value::as_object)
                .ok_or_else(|| MockAnswerError::Registry("no `services` object".to_string()))?;
            let mut out = Shapes::new();
            for (name, service) in services {
                let mut actions = HashMap::new();
                let list = service
                    .get("actions")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                for action in list {
                    let action_name =
                        action.get("name").and_then(Value::as_str).ok_or_else(|| {
                            MockAnswerError::Registry(format!("{name}: an action has no name"))
                        })?;
                    let shape = match action.get("responseShape") {
                        None | Some(Value::Null) => Map::new(),
                        Some(Value::Object(m)) => m.clone(),
                        Some(_) => {
                            return Err(MockAnswerError::Registry(format!(
                                "{name}.{action_name}: responseShape is not an object"
                            )))
                        }
                    };
                    actions.insert(action_name.to_string(), shape);
                }
                out.insert(name.clone(), actions);
            }
            Ok(out)
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// The mock answer for one `service.action` call: the registry-shaped value, or
/// `{success: true, _mock: true, _action}` for an action that declares no shape.
pub fn mock_answer(
    service: &str,
    action: &str,
    params: &Map<String, Value>,
) -> Result<Value, MockAnswerError> {
    let actions = shapes()?
        .get(service)
        .ok_or_else(|| MockAnswerError::UnknownService(service.to_string()))?;
    let shape = actions
        .get(action)
        .ok_or_else(|| MockAnswerError::UnknownAction {
            service: service.to_string(),
            action: action.to_string(),
        })?;
    if shape.is_empty() {
        return Ok(json!({"success": true, "_mock": true, "_action": action}));
    }
    let malformed = |field: &str| MockAnswerError::MalformedShape {
        service: service.to_string(),
        action: action.to_string(),
        field: field.to_string(),
    };
    let mut data = Map::new();
    for (key, descriptor) in shape {
        let value =
            mock_value_for_descriptor(key, descriptor, params).ok_or_else(|| malformed(key))?;
        data.insert(key.clone(), value);
    }
    Ok(Value::Object(data))
}

/// A mock value for a JSON-schema node (what a `shapedBy` param declares). `None` is an absent param.
pub fn mock_from_json_schema(key: &str, schema: Option<&Value>) -> Value {
    let Some(schema) = schema.and_then(Value::as_object) else {
        return json!({});
    };
    if let Some(Value::String(first)) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    {
        return Value::String(first.clone());
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let mut obj = Map::new();
            if let Some(Value::Object(props)) = schema.get("properties") {
                for (k, s) in props {
                    obj.insert(k.clone(), mock_from_json_schema(k, Some(s)));
                }
            }
            Value::Object(obj)
        }
        Some("array") => match schema.get("items") {
            None => json!([]),
            Some(items) => Value::Array(vec![mock_from_json_schema(
                &format!("{key}Item"),
                Some(items),
            )]),
        },
        Some("string") => Value::String(format!("mock_{key}")),
        Some("number") | Some("integer") => json!(42),
        Some("boolean") => json!(true),
        _ => json!({}),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// A mock value for one `responseShape` field. `params` shapes only a top-level `shapedBy`
/// field, as in TS (a nested descriptor is mocked without the request). `None` means the
/// descriptor is not a shape (no string `type`, `properties` not an object).
pub fn mock_value_for_descriptor(
    key: &str,
    descriptor: &Value,
    params: &Map<String, Value>,
) -> Option<Value> {
    mock_value(key, descriptor, Some(params))
}

fn mock_value(key: &str, descriptor: &Value, params: Option<&Map<String, Value>>) -> Option<Value> {
    match descriptor {
        Value::String(s) => Some(flat(key, s)),
        Value::Object(d) => {
            if let Some(shaped_by) = d.get("shapedBy").and_then(Value::as_str) {
                return Some(mock_from_json_schema(
                    key,
                    params.and_then(|p| p.get(shaped_by)),
                ));
            }
            let ty = d.get("type").and_then(Value::as_str)?;
            let (items, properties, map_value) =
                (d.get("items"), d.get("properties"), d.get("mapValue"));
            if items.is_none() && properties.is_none() && map_value.is_none() {
                return Some(flat(key, ty));
            }
            if ty == "array" || ty.ends_with("[]") {
                return match items.filter(|i| truthy(i)) {
                    Some(i) => Some(Value::Array(vec![mock_value(
                        &format!("{key}Item"),
                        i,
                        None,
                    )?])),
                    None => Some(json!([])),
                };
            }
            if let Some(p) = properties.filter(|p| truthy(p)) {
                let mut obj = Map::new();
                for (k, d) in p.as_object()? {
                    obj.insert(k.clone(), mock_value(k, d, None)?);
                }
                return Some(Value::Object(obj));
            }
            if let Some(m) = map_value.filter(|m| truthy(m)) {
                let mut obj = Map::new();
                obj.insert("sample".to_string(), mock_value("sample", m, None)?);
                return Some(Value::Object(obj));
            }
            Some(json!({}))
        }
        _ => None,
    }
}

fn flat(key: &str, ty: &str) -> Value {
    if ty.ends_with("[]") {
        return json!([]);
    }
    match ty {
        "number" => json!(42),
        "boolean" => json!(true),
        "object" => json!({}),
        _ => Value::String(format!("mock_{key}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Map<String, Value> {
        Map::new()
    }

    #[test]
    fn flat_spellings() {
        let d = |v: Value| mock_value_for_descriptor("f", &v, &p()).unwrap();
        assert_eq!(d(json!("string")), json!("mock_f"));
        assert_eq!(d(json!("number | null")), json!("mock_f"));
        assert_eq!(d(json!("Row[]")), json!([]));
        assert_eq!(d(json!("object")), json!({}));
        assert_eq!(d(json!("boolean")), json!(true));
    }

    #[test]
    fn nested_array_of_objects_and_map() {
        let d = json!({"type": "array", "items": {"type": "X", "properties": {"a": "string", "n": "number"}}});
        assert_eq!(
            mock_value_for_descriptor("rows", &d, &p()).unwrap(),
            json!([{"a": "mock_a", "n": 42}])
        );
        let m = json!({"type": "object", "mapValue": "number"});
        assert_eq!(
            mock_value_for_descriptor("m", &m, &p()).unwrap(),
            json!({"sample": 42})
        );
    }

    #[test]
    fn control_array_without_items_is_empty_and_malformed_is_none() {
        assert_eq!(
            mock_value_for_descriptor("f", &json!({"type": "array", "properties": {}}), &p()),
            Some(json!([]))
        );
        assert_eq!(
            mock_value_for_descriptor("f", &json!({"items": "x"}), &p()),
            None
        );
        assert_eq!(mock_value_for_descriptor("f", &json!(5), &p()), None);
    }

    #[test]
    fn shaped_by_reads_the_named_param_only_at_top_level() {
        let d = json!({"type": "T", "shapedBy": "schema"});
        let mut params = Map::new();
        params.insert("schema".into(), json!({"type": "string"}));
        assert_eq!(
            mock_value_for_descriptor("k", &d, &params).unwrap(),
            json!("mock_k")
        );
        assert_eq!(mock_value_for_descriptor("k", &d, &p()).unwrap(), json!({}));
        let nested = json!({"type": "A", "properties": {"inner": d}});
        assert_eq!(
            mock_value_for_descriptor("k", &nested, &params).unwrap(),
            json!({"inner": {}})
        );
    }
}
