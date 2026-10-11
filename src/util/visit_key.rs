//! The one identity of a cascade delivery (Runtime Spec Clause 5.3): `(trait, event, from, payload, frame)`.
//! Twin of `@almadar/core` `dispatchVisitKey`; both runtimes must agree on which deliveries are repeats.

use serde::Serialize;
use serde_json::{Map, Number, Value};
use std::collections::HashMap;

use crate::evaluator::Value as EvalValue;

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
        Value::Number(n) => Value::Number(canonical_number(n)),
        other => other.clone(),
    }
}

fn canonical_number(n: &Number) -> Number {
    match n.as_f64() {
        Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() <= MAX_SAFE_INTEGER => {
            Number::from(f as i64)
        }
        _ => n.clone(),
    }
}

/// `canonical_json(a) == canonical_json(b)` on two JSON trees, without building either string.
fn same_canonical(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| same_canonical(v, w)))
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(v, w)| same_canonical(v, w))
        }
        (Value::Number(x), Value::Number(y)) => canonical_number(x) == canonical_number(y),
        _ => a == b,
    }
}

/// `canonical_json(a) == canonical_json(b)` on two evaluator values (a non-finite number
/// serializes as null; a lambda as its s-expression).
fn same_value(a: &EvalValue, b: &EvalValue) -> bool {
    match (a, b) {
        (EvalValue::Lambda { .. }, _) | (_, EvalValue::Lambda { .. }) => same_canonical(
            &serde_json::to_value(a).unwrap_or(Value::Null),
            &serde_json::to_value(b).unwrap_or(Value::Null),
        ),
        (EvalValue::Number(x), EvalValue::Number(y)) => {
            if x.is_finite() && y.is_finite() {
                x == y
            } else {
                !x.is_finite() && !y.is_finite()
            }
        }
        (EvalValue::Number(n), EvalValue::Null) | (EvalValue::Null, EvalValue::Number(n)) => {
            !n.is_finite()
        }
        (EvalValue::Object(x), EvalValue::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| same_value(v, w)))
        }
        (EvalValue::Array(x), EvalValue::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(v, w)| same_value(v, w))
        }
        _ => a == b,
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

/// A set of deliveries under the `dispatch_visit_key` identity. Steps are filed by
/// `(trait, event, from)`; payload and frame are compared only against earlier visits of that
/// same step, so a large entity frame is not serialized on every cascade step.
#[derive(Debug, Default)]
pub struct DispatchVisits {
    steps: HashMap<String, Vec<(EvalValue, EvalValue)>>,
}

impl DispatchVisits {
    pub fn contains(
        &self,
        trait_name: &str,
        event: &str,
        from: &str,
        payload: &EvalValue,
        frame: &EvalValue,
    ) -> bool {
        self.steps
            .get(&format!("{trait_name}\u{0}{event}\u{0}{from}"))
            .is_some_and(|seen| {
                seen.iter()
                    .any(|(p, f)| same_value(p, payload) && same_value(f, frame))
            })
    }

    /// `true` when the step was not visited before (as `HashSet::insert`).
    pub fn insert(
        &mut self,
        trait_name: &str,
        event: &str,
        from: &str,
        payload: EvalValue,
        frame: EvalValue,
    ) -> bool {
        if self.contains(trait_name, event, from, &payload, &frame) {
            return false;
        }
        self.steps
            .entry(format!("{trait_name}\u{0}{event}\u{0}{from}"))
            .or_default()
            .push((payload, frame));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::Value as EValue;
    use crate::schema::SExpression;
    use serde_json::json;
    use std::sync::Arc;

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

    fn ev(v: Value) -> EValue {
        fn conv(v: Value) -> EValue {
            match v {
                Value::Null => EValue::Null,
                Value::Bool(b) => EValue::Bool(b),
                Value::Number(n) => EValue::Number(n.as_f64().unwrap_or(f64::NAN)),
                Value::String(s) => EValue::String(s),
                Value::Array(a) => EValue::Array(a.into_iter().map(conv).collect()),
                Value::Object(o) => {
                    EValue::Object(o.into_iter().map(|(k, v)| (k, conv(v))).collect())
                }
            }
        }
        conv(v)
    }

    fn lambda(param: &str) -> EValue {
        EValue::Lambda {
            params: vec![param.to_string()],
            body: Arc::new(SExpression::String(param.to_string())),
        }
    }

    fn corpus() -> Vec<EValue> {
        let mut values: Vec<EValue> = [
            Value::Null,
            json!(0),
            json!(1),
            json!(1.5),
            json!(-1),
            json!(""),
            json!("null"),
            json!("1"),
            json!(true),
            json!(false),
            json!([]),
            json!([null]),
            json!([1, 2]),
            json!([2, 1]),
            json!({}),
            json!({ "a": null }),
            json!({ "a": 1 }),
            json!({ "b": 2, "a": 1 }),
            json!({ "a": 1, "b": 2 }),
            json!({ "a": [1, { "c": 2 }] }),
            json!({ "a": [1, { "c": 3 }] }),
        ]
        .into_iter()
        .map(ev)
        .collect();
        values.extend([
            EValue::Number(-0.0),
            EValue::Number(f64::NAN),
            EValue::Number(f64::INFINITY),
            EValue::Number(1e300),
            EValue::Number(9_007_199_254_740_993.0),
            lambda("x"),
            lambda("y"),
            ev(json!(["fn", ["x"], "x"])),
            EValue::Array(vec![lambda("x")]),
        ]);
        values
    }

    #[test]
    fn visits_agree_with_the_key_on_every_payload_pair() {
        for a in corpus() {
            for b in corpus() {
                let mut visits = DispatchVisits::default();
                visits.insert("T", "E", "s", a.clone(), EValue::Null);
                assert_eq!(
                    visits.contains("T", "E", "s", &b, &EValue::Null),
                    dispatch_visit_key("T", "E", "s", &a, &EValue::Null)
                        == dispatch_visit_key("T", "E", "s", &b, &EValue::Null),
                    "{a:?} vs {b:?}"
                );
            }
        }
    }

    #[test]
    fn visits_agree_with_the_key_on_every_frame_pair() {
        for a in corpus() {
            for b in corpus() {
                let mut visits = DispatchVisits::default();
                visits.insert("T", "E", "s", EValue::Null, a.clone());
                assert_eq!(
                    visits.contains("T", "E", "s", &EValue::Null, &b),
                    dispatch_visit_key("T", "E", "s", &EValue::Null, &a)
                        == dispatch_visit_key("T", "E", "s", &EValue::Null, &b),
                    "{a:?} vs {b:?}"
                );
            }
        }
    }

    #[test]
    fn visits_keep_trait_event_and_from_in_the_identity() {
        let mut visits = DispatchVisits::default();
        assert!(visits.insert("T", "E", "s", EValue::Null, EValue::Null));
        assert!(!visits.contains("U", "E", "s", &EValue::Null, &EValue::Null));
        assert!(!visits.contains("T", "F", "s", &EValue::Null, &EValue::Null));
        assert!(!visits.contains("T", "E", "t", &EValue::Null, &EValue::Null));
        assert!(!visits.contains("a\u{0}b", "c", "s", &EValue::Null, &EValue::Null));
    }

    #[test]
    fn visits_report_the_identical_step_as_a_repeat() {
        let mut visits = DispatchVisits::default();
        assert!(visits.insert("T", "E", "s", ev(json!({ "k": 1 })), ev(json!({ "n": 1 }))));
        assert!(visits.contains(
            "T",
            "E",
            "s",
            &ev(json!({ "k": 1 })),
            &ev(json!({ "n": 1 }))
        ));
        assert!(!visits.insert("T", "E", "s", ev(json!({ "k": 1 })), ev(json!({ "n": 1 }))));
    }

    #[test]
    fn visits_remember_every_distinct_frame_of_one_step() {
        let mut visits = DispatchVisits::default();
        assert!(visits.insert("T", "E", "s", EValue::Null, ev(json!({ "n": 1 }))));
        assert!(visits.insert("T", "E", "s", EValue::Null, ev(json!({ "n": 2 }))));
        assert!(visits.contains("T", "E", "s", &EValue::Null, &ev(json!({ "n": 1 }))));
        assert!(visits.contains("T", "E", "s", &EValue::Null, &ev(json!({ "n": 2 }))));
        assert!(!visits.contains("T", "E", "s", &EValue::Null, &ev(json!({ "n": 3 }))));
    }
}
