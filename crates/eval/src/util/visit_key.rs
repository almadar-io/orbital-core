//! The one identity of a cascade delivery (Runtime Spec Clause 5.3): `(trait, event, from, payload, frame)`.
//! Twin of `@almadar/core` `dispatchVisitKey`; both runtimes must agree on which deliveries are repeats.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::Serialize;
use serde_json::{Map, Number, Value};
use xxhash_rust::xxh3::Xxh3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use orbital_core_base::value::Value as EvalValue;

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

/// A trait's frame as the cycle check reads it: none, its fields as the kernel holds them (each behind its own `Arc`),
/// or another value.
pub enum VisitFrame<'a> {
    Null,
    Fields(Vec<(&'a str, &'a Arc<EvalValue>)>),
    Value(&'a EvalValue),
}

impl<'a> VisitFrame<'a> {
    /// The frame of an entity as held, never copied.
    pub fn of(entity: &'a orbital_core_base::shared_entity::SharedEntity) -> Self {
        use orbital_core_base::shared_entity::EntityParts;
        match entity.parts() {
            EntityParts::Null => VisitFrame::Null,
            EntityParts::Fields(fields) => VisitFrame::Fields(fields.iter().map(|(k, v)| (k.as_str(), v)).collect()),
            EntityParts::Value(v) => VisitFrame::Value(v),
        }
    }
}

/// A set of deliveries under the `dispatch_visit_key` identity. Steps are filed by
/// `(trait, event, from)` and each visit keeps only the xxh3-128 of its canonical payload and
/// frame, so a long loop over a large entity does not retain a copy of every frame it passed.
/// A frame field's digest is kept for the value the field holds, so a field a step left
/// unchanged is not digested again.
#[derive(Debug, Default)]
pub struct DispatchVisits {
    steps: HashMap<String, HashSet<u128>>,
    fields: HashMap<String, (Arc<EvalValue>, u128)>,
}

/// One step's identity, computed once: its `(trait, event, from)` and the digest of its payload and frame. A step asks
/// [`DispatchVisits::seen`] before it runs and [`DispatchVisits::record`]s itself after, with the same `Visit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visit {
    step: String,
    digest: u128,
}

impl DispatchVisits {
    /// The identity of a step, digesting its payload and frame once.
    pub fn visit(&mut self, trait_name: &str, event: &str, from: &str, payload: &EvalValue, frame: &VisitFrame<'_>) -> Visit {
        Visit {
            step: format!("{trait_name}\u{0}{event}\u{0}{from}"),
            digest: self.visit_digest(payload, frame),
        }
    }

    pub fn seen(&self, visit: &Visit) -> bool {
        self.steps.get(&visit.step).is_some_and(|seen| seen.contains(&visit.digest))
    }

    /// `true` when the step was not visited before (as `HashSet::insert`).
    pub fn record(&mut self, visit: Visit) -> bool {
        self.steps.entry(visit.step).or_default().insert(visit.digest)
    }

    pub fn contains(&mut self, trait_name: &str, event: &str, from: &str, payload: &EvalValue, frame: &VisitFrame<'_>) -> bool {
        let visit = self.visit(trait_name, event, from, payload, frame);
        self.seen(&visit)
    }

    /// `true` when the step was not visited before (as `HashSet::insert`).
    pub fn insert(&mut self, trait_name: &str, event: &str, from: &str, payload: &EvalValue, frame: &VisitFrame<'_>) -> bool {
        let visit = self.visit(trait_name, event, from, payload, frame);
        self.record(visit)
    }

    /// xxh3-128 over an injective encoding of the canonical payload and the frame: a field enters as the digest of
    /// its canonical value, so equal frames still digest equal and differing ones differ.
    fn visit_digest(&mut self, payload: &EvalValue, frame: &VisitFrame<'_>) -> u128 {
        let mut h = Xxh3::new();
        digest_value(&mut h, payload);
        match frame {
            VisitFrame::Null => h.update(b"n"),
            VisitFrame::Value(value) => {
                h.update(b"v");
                digest_value(&mut h, value);
            }
            VisitFrame::Fields(fields) => {
                h.update(b"o");
                digest_len(&mut h, fields.len());
                let mut sorted: Vec<&(&str, &Arc<EvalValue>)> = fields.iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(b.0));
                for (name, value) in sorted {
                    digest_str(&mut h, name);
                    h.update(&self.field_digest(name, value).to_le_bytes());
                }
            }
        }
        h.digest128()
    }

    fn field_digest(&mut self, name: &str, value: &Arc<EvalValue>) -> u128 {
        if let Some((held, digest)) = self.fields.get(name) {
            if Arc::ptr_eq(held, value) {
                return *digest;
            }
        }
        let mut h = Xxh3::new();
        digest_value(&mut h, value);
        let digest = h.digest128();
        self.fields.insert(name.to_string(), (Arc::clone(value), digest));
        digest
    }
}

fn digest_len(h: &mut Xxh3, n: usize) {
    h.update(&(n as u64).to_le_bytes());
}

fn digest_str(h: &mut Xxh3, s: &str) {
    digest_len(h, s.len());
    h.update(s.as_bytes());
}

/// A number by its canonical value (an integral float within the safe range is that integer), hashed from its bits.
fn digest_number(h: &mut Xxh3, n: &Number) {
    match (n.as_i64(), n.as_u64(), n.as_f64()) {
        (Some(i), _, _) if !n.is_f64() => digest_integer(h, i),
        (_, Some(u), _) if !n.is_f64() => {
            h.update(b"#u");
            h.update(&u.to_le_bytes());
        }
        (_, _, Some(f)) => digest_float(h, f),
        _ => h.update(b"n"),
    }
}

fn digest_integer(h: &mut Xxh3, i: i64) {
    h.update(b"#i");
    h.update(&i.to_le_bytes());
}

fn digest_float(h: &mut Xxh3, f: f64) {
    if f.fract() == 0.0 && f.abs() <= MAX_SAFE_INTEGER {
        digest_integer(h, f as i64);
    } else {
        h.update(b"#f");
        h.update(&f.to_bits().to_le_bytes());
    }
}

fn digest_value(h: &mut Xxh3, value: &EvalValue) {
    match value {
        EvalValue::Lambda { .. } => {
            digest_json(h, &serde_json::to_value(value).unwrap_or(Value::Null))
        }
        EvalValue::Null => h.update(b"n"),
        EvalValue::Bool(b) => h.update(if *b { b"t" } else { b"f" }),
        EvalValue::Number(n) if n.is_finite() => digest_float(h, *n),
        EvalValue::Number(_) => h.update(b"n"),
        EvalValue::String(s) => {
            h.update(b"s");
            digest_str(h, s);
        }
        EvalValue::Array(items) => {
            h.update(b"a");
            digest_len(h, items.len());
            items.iter().for_each(|v| digest_value(h, v));
        }
        EvalValue::Object(map) => {
            h.update(b"o");
            digest_len(h, map.len());
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                digest_str(h, k);
                digest_value(h, &map[k]);
            }
        }
    }
}

fn digest_json(h: &mut Xxh3, value: &Value) {
    match value {
        Value::Null => h.update(b"n"),
        Value::Bool(b) => h.update(if *b { b"t" } else { b"f" }),
        Value::Number(n) => digest_number(h, n),
        Value::String(s) => {
            h.update(b"s");
            digest_str(h, s);
        }
        Value::Array(items) => {
            h.update(b"a");
            digest_len(h, items.len());
            items.iter().for_each(|v| digest_json(h, v));
        }
        Value::Object(map) => {
            h.update(b"o");
            digest_len(h, map.len());
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                digest_str(h, k);
                digest_json(h, &map[k]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbital_core_base::schema::sexpr::SExpression;
    use orbital_core_base::value::Value as EValue;
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
                Value::String(s) => EValue::String(s.into()),
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
            body: Arc::new(SExpression::string(param.to_string())),
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
            EValue::Array(vec![lambda("x")].into()),
        ]);
        values
    }

    #[test]
    fn visits_agree_with_the_key_on_every_payload_pair() {
        for a in corpus() {
            for b in corpus() {
                let mut visits = DispatchVisits::default();
                visits.insert("T", "E", "s", &a, &frame(&EValue::Null));
                assert_eq!(
                    visits.contains("T", "E", "s", &b, &frame(&EValue::Null)),
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
                visits.insert("T", "E", "s", &EValue::Null, &frame(&a));
                assert_eq!(
                    visits.contains("T", "E", "s", &EValue::Null, &frame(&b)),
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
        assert!(visits.insert("T", "E", "s", &EValue::Null, &frame(&EValue::Null)));
        assert!(!visits.contains("U", "E", "s", &EValue::Null, &frame(&EValue::Null)));
        assert!(!visits.contains("T", "F", "s", &EValue::Null, &frame(&EValue::Null)));
        assert!(!visits.contains("T", "E", "t", &EValue::Null, &frame(&EValue::Null)));
        assert!(!visits.contains("a\u{0}b", "c", "s", &EValue::Null, &frame(&EValue::Null)));
    }

    #[test]
    fn visits_report_the_identical_step_as_a_repeat() {
        let mut visits = DispatchVisits::default();
        assert!(visits.insert("T", "E", "s", &ev(json!({ "k": 1 })), &frame(&ev(json!({ "n": 1 })))));
        assert!(visits.contains(
            "T",
            "E",
            "s",
            &ev(json!({ "k": 1 })),
            &frame(&ev(json!({ "n": 1 })))
        ));
        assert!(!visits.insert("T", "E", "s", &ev(json!({ "k": 1 })), &frame(&ev(json!({ "n": 1 })))));
    }

    #[test]
    fn visits_hold_no_copy_of_a_payload_or_frame() {
        let body = Arc::new(SExpression::string("x"));
        let held = || EValue::Lambda {
            params: vec!["x".to_string()],
            body: Arc::clone(&body),
        };
        let mut visits = DispatchVisits::default();
        assert!(visits.insert("T", "E", "s", &held(), &frame(&EValue::Array(vec![held(), held()].into()))));
        assert_eq!(Arc::strong_count(&body), 1);
        assert!(visits.contains("T", "E", "s", &held(), &frame(&EValue::Array(vec![held(), held()].into()))));
    }

    #[test]
    fn visits_remember_every_distinct_frame_of_one_step() {
        let mut visits = DispatchVisits::default();
        assert!(visits.insert("T", "E", "s", &EValue::Null, &frame(&ev(json!({ "n": 1 })))));
        assert!(visits.insert("T", "E", "s", &EValue::Null, &frame(&ev(json!({ "n": 2 })))));
        assert!(visits.contains("T", "E", "s", &EValue::Null, &frame(&ev(json!({ "n": 1 })))));
        assert!(visits.contains("T", "E", "s", &EValue::Null, &frame(&ev(json!({ "n": 2 })))));
        assert!(!visits.contains("T", "E", "s", &EValue::Null, &frame(&ev(json!({ "n": 3 })))));
    }

    fn frame(value: &EValue) -> VisitFrame<'_> {
        VisitFrame::Value(value)
    }

    fn fields<'a>(pairs: &'a [(&'a str, Arc<EValue>)]) -> VisitFrame<'a> {
        VisitFrame::Fields(pairs.iter().map(|(k, v)| (*k, v)).collect())
    }

    #[test]
    fn a_fields_frame_is_one_visit_whatever_holds_its_equal_values() {
        let mut visits = DispatchVisits::default();
        let first = [("a", Arc::new(ev(json!([1, 2])))), ("b", Arc::new(ev(json!("x"))))];
        let again = [("b", Arc::new(ev(json!("x")))), ("a", Arc::new(ev(json!([1, 2]))))];
        assert!(visits.insert("T", "E", "s", &EValue::Null, &fields(&first)));
        assert!(visits.contains("T", "E", "s", &EValue::Null, &fields(&again)), "equal fields in new Arcs and another order");
        let moved = [("a", Arc::new(ev(json!([1, 2, 3])))), ("b", Arc::clone(&first[1].1))];
        assert!(!visits.contains("T", "E", "s", &EValue::Null, &fields(&moved)), "control: a changed field is a new frame");
    }

    #[test]
    fn edge_a_field_keeps_its_digest_only_for_the_value_it_holds() {
        let mut visits = DispatchVisits::default();
        let large = Arc::new(ev(json!({ "rows": [1, 2, 3] })));
        let step = |n: i64| [("large", Arc::clone(&large)), ("n", Arc::new(ev(json!(n))))];
        assert!(visits.insert("T", "E", "s", &EValue::Null, &fields(&step(1))));
        assert!(visits.insert("T", "E", "s", &EValue::Null, &fields(&step(2))));
        assert_eq!(Arc::strong_count(&large), 2, "the digest cache holds the one value it digested, no copy per step");
        assert!(visits.contains("T", "E", "s", &EValue::Null, &fields(&step(1))));
        assert!(!visits.contains("T", "E", "s", &EValue::Null, &frame(&EValue::Null)), "control: no fields is another frame");
    }

    #[test]
    fn a_number_is_one_payload_whatever_its_spelling_and_another_number_is_not() {
        let num = |n: f64| EValue::Number(n);
        let mut visits = DispatchVisits::default();
        assert!(visits.insert("T", "E", "s", &num(1.0), &frame(&EValue::Null)));
        assert!(visits.contains("T", "E", "s", &ev(json!(1)), &frame(&EValue::Null)), "1 and 1.0 are one payload");
        assert!(!visits.contains("T", "E", "s", &num(2.0), &frame(&EValue::Null)), "control: 2 is another payload");
        assert!(!visits.contains("T", "E", "s", &num(1.5), &frame(&EValue::Null)), "control: 1.5 is another payload");
        assert!(!visits.contains("T", "E", "s", &EValue::string("1"), &frame(&EValue::Null)), "control: the text 1 is not the number");
        assert!(visits.insert("T", "E", "s", &num(0.1 + 0.2), &frame(&EValue::Null)));
        assert!(!visits.contains("T", "E", "s", &num(0.3), &frame(&EValue::Null)), "edge: distinct floats stay distinct");
        assert!(visits.insert("T", "E", "s", &num(9_007_199_254_740_993.0), &frame(&EValue::Null)));
        assert!(visits.insert("T", "E", "s", &num(-0.0), &frame(&EValue::Null)));
        assert!(visits.contains("T", "E", "s", &num(0.0), &frame(&EValue::Null)), "edge: -0 and 0 are one payload");
    }
}
