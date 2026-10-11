//! The one identity of a cascade delivery (Runtime Spec Clause 5.3): `(trait, event, from, payload, frame)`.
//! Twin of `@almadar/core` `dispatchVisitKey`; both runtimes must agree on which deliveries are repeats.

use serde::Serialize;
use serde_json::{Map, Number, Value};

/// Largest integer an `f64` holds exactly; beyond it an integral float stays a float.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Sorted keys, integral floats as integers, so `{b:1.0,a:2}` and `{a:2,b:1}` are one payload.
fn canonical_payload(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut sorted = Map::new();
            for k in keys {
                sorted.insert(k.clone(), canonical_payload(&map[k]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_payload).collect()),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() <= MAX_SAFE_INTEGER => {
                Value::Number(Number::from(f as i64))
            }
            _ => value.clone(),
        },
        other => other.clone(),
    }
}

fn canonical_json(value: &impl Serialize) -> String {
    let value = serde_json::to_value(value).unwrap_or(Value::Null);
    serde_json::to_string(&canonical_payload(&value)).unwrap_or_else(|_| "null".to_string())
}

/// `payload` is null when the delivery carries none; `frame` is the trait's entity frame as it
/// stands when the step is about to run (null when it has none). A true cycle repeats the frame
/// too, so a loop whose frame moves each round is progress.
pub fn dispatch_visit_key(
    trait_name: &str,
    event: &str,
    from: &str,
    payload: &impl Serialize,
    frame: &impl Serialize,
) -> String {
    let payload = canonical_json(payload);
    let frame = canonical_json(frame);
    format!("{trait_name}\u{0}{event}\u{0}{from}\u{0}{payload}\u{0}{frame}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(p: Value) -> String {
        dispatch_visit_key("Search", "STEP", "searching", &p, &Value::Null)
    }

    #[test]
    fn object_key_order_does_not_change_the_key() {
        assert_eq!(
            key(json!({ "a": 1, "b": { "c": 2, "d": 3 } })),
            key(json!({ "b": { "d": 3, "c": 2 }, "a": 1 }))
        );
    }

    #[test]
    fn array_order_does_change_the_key() {
        assert_ne!(key(json!({ "ids": [1, 2] })), key(json!({ "ids": [2, 1] })));
    }

    #[test]
    fn an_integral_float_is_the_same_number_as_the_integer() {
        assert_eq!(
            key(json!({ "remaining": 1.0 })),
            key(json!({ "remaining": 1 }))
        );
        assert_ne!(
            key(json!({ "remaining": 1.5 })),
            key(json!({ "remaining": 1 }))
        );
    }

    #[test]
    fn different_payloads_are_different_deliveries() {
        assert_ne!(
            key(json!({ "remaining": 2 })),
            key(json!({ "remaining": 1 }))
        );
        assert_ne!(key(Value::Null), key(json!({})));
    }

    #[test]
    fn each_component_is_part_of_the_identity() {
        let base = dispatch_visit_key("T", "E", "s", &Value::Null, &Value::Null);
        assert_ne!(
            base,
            dispatch_visit_key("U", "E", "s", &Value::Null, &Value::Null)
        );
        assert_ne!(
            base,
            dispatch_visit_key("T", "F", "s", &Value::Null, &Value::Null)
        );
        assert_ne!(
            base,
            dispatch_visit_key("T", "E", "t", &Value::Null, &Value::Null)
        );
    }

    #[test]
    fn a_colon_in_a_name_cannot_collide_across_components() {
        assert_ne!(
            dispatch_visit_key("a:b", "c", "s", &Value::Null, &Value::Null),
            dispatch_visit_key("a", "b:c", "s", &Value::Null, &Value::Null)
        );
    }

    fn framed(frame: Value) -> String {
        dispatch_visit_key("T", "E", "s", &json!({ "totalCount": 12 }), &frame)
    }

    #[test]
    fn a_different_frame_is_a_different_delivery() {
        assert_ne!(framed(json!({ "batch": 1 })), framed(json!({ "batch": 2 })));
    }

    #[test]
    fn an_identical_frame_is_the_same_delivery() {
        assert_eq!(framed(json!({ "batch": 1 })), framed(json!({ "batch": 1 })));
    }

    #[test]
    fn frame_key_order_does_not_change_the_key() {
        assert_eq!(
            framed(json!({ "a": 1, "b": { "c": 2, "d": 3 } })),
            framed(json!({ "b": { "d": 3, "c": 2 }, "a": 1 }))
        );
    }

    #[test]
    fn a_null_frame_and_an_empty_frame_are_different() {
        assert_ne!(framed(Value::Null), framed(json!({})));
    }

    #[test]
    fn a_frame_cannot_masquerade_as_a_payload() {
        let a = dispatch_visit_key("T", "E", "s", &json!(1), &json!(2));
        let b = dispatch_visit_key("T", "E", "s", &json!(2), &json!(1));
        assert_ne!(a, b);
    }
}
