//! The one identity of a cascade delivery (Runtime Spec Clause 5.3): `(trait, event, from, payload, frame)`.
//! Twin of `@almadar/core` `dispatchVisitKey`; both runtimes must agree on which deliveries are repeats.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use serde::Serialize;
use serde_json::{Map, Number, Value};
use xxhash_rust::xxh3::Xxh3;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

use orbital_core_base::sync::{LazyLock, Mutex};

use orbital_core_base::value::{Shared, Value as EvalValue};

/// Largest integer an `f64` holds exactly; beyond it an integral float stays a float.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Sorted keys, integral floats as integers, so `{b:1.0,a:2}` and `{a:2,b:1}` are one payload.
fn canonical_payload(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
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
/// A frame field's digest is kept per trait for the value the field holds, so a field a step left
/// unchanged is not digested again and two traits with one field name do not evict each other.
#[derive(Debug, Default)]
pub struct DispatchVisits {
    steps: HashMap<String, HashSet<u128>>,
    fields: HashMap<String, HashMap<String, (Arc<EvalValue>, u128)>>,
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
        self.visit_digested(trait_name, event, from, payload_digest(payload), frame)
    }

    /// [`Self::visit`] for a payload whose [`payload_digest`] the caller holds: a fan-out digests the one payload its
    /// listeners share once.
    pub fn visit_digested(&mut self, trait_name: &str, event: &str, from: &str, payload: u128, frame: &VisitFrame<'_>) -> Visit {
        Visit {
            step: format!("{trait_name}\u{0}{event}\u{0}{from}"),
            digest: self.visit_digest(trait_name, payload, frame),
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
    fn visit_digest(&mut self, trait_name: &str, payload: u128, frame: &VisitFrame<'_>) -> u128 {
        let mut h = Xxh3::new();
        h.update(&payload.to_le_bytes());
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
                    h.update(&self.field_digest(trait_name, name, value).to_le_bytes());
                }
            }
        }
        h.digest128()
    }

    fn field_digest(&mut self, trait_name: &str, name: &str, value: &Arc<EvalValue>) -> u128 {
        if let Some((held, digest)) = self.fields.get(trait_name).and_then(|fields| fields.get(name)) {
            if Arc::ptr_eq(held, value) {
                return *digest;
            }
        }
        let mut h = Xxh3::new();
        digest_value(&mut h, value);
        let digest = h.digest128();
        match self.fields.get_mut(trait_name) {
            Some(fields) => {
                fields.insert(name.to_string(), (Arc::clone(value), digest));
            }
            None => {
                self.fields.insert(trait_name.to_string(), HashMap::from([(name.to_string(), (Arc::clone(value), digest))]));
            }
        }
        digest
    }
}

/// The xxh3-128 of a payload's canonical encoding, as a visit folds it in.
pub fn payload_digest(payload: &EvalValue) -> u128 {
    let mut h = Xxh3::new();
    digest_value(&mut h, payload);
    h.digest128()
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

/// Bytes values at least this long keep their digest by allocation ([`bytes_digest`]).
const BYTES_MEMO_MIN: usize = 4096;

struct BytesDigests {
    by_address: HashMap<usize, (Weak<Vec<u8>>, u128)>,
    prune_at: usize,
}

static BYTES_DIGESTS: LazyLock<Mutex<BytesDigests>> =
    LazyLock::new(|| Mutex::new(BytesDigests { by_address: HashMap::new(), prune_at: 1024 }));

fn bytes_address(bytes: &Shared<Vec<u8>>) -> usize {
    let held: &Vec<u8> = bytes;
    held as *const Vec<u8> as usize
}

/// The xxh3-128 of a bytes value. A large one is digested once per allocation for the life of the process: values are
/// immutable, and the memo holds only a weak handle, so a freed allocation whose address is reused never matches.
fn bytes_digest(bytes: &Shared<Vec<u8>>) -> u128 {
    if bytes.len() < BYTES_MEMO_MIN {
        return xxhash_rust::xxh3::xxh3_128(bytes);
    }
    let at = bytes_address(bytes);
    if let Some((held, digest)) = BYTES_DIGESTS.lock().by_address.get(&at) {
        if held.upgrade().is_some_and(|live| (&*live as *const Vec<u8> as usize) == at) {
            return *digest;
        }
    }
    let digest = xxhash_rust::xxh3::xxh3_128(bytes);
    let mut memo = BYTES_DIGESTS.lock();
    memo.by_address.insert(at, (Shared::downgrade(bytes), digest));
    if memo.by_address.len() > memo.prune_at {
        memo.by_address.retain(|_, (held, _)| held.strong_count() > 0);
        memo.prune_at = (memo.by_address.len() * 2).max(1024);
    }
    digest
}

/// Arrays and objects with at least this many entries keep their digest by allocation ([`container_digest`]).
const CONTAINER_MEMO_MIN: usize = 8;

struct ContainerDigests {
    arrays: HashMap<usize, (Weak<Vec<EvalValue>>, u128)>,
    objects: HashMap<usize, (Weak<orbital_core_base::value::ValueMap>, u128)>,
    prune_at: usize,
}

static CONTAINER_DIGESTS: LazyLock<Mutex<ContainerDigests>> =
    LazyLock::new(|| Mutex::new(ContainerDigests { arrays: HashMap::new(), objects: HashMap::new(), prune_at: 4096 }));

#[cfg(test)]
thread_local! {
    static CONTAINER_DIGESTS_COMPUTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn container_digests_computed() -> usize {
    CONTAINER_DIGESTS_COMPUTED.with(std::cell::Cell::get)
}

fn prune_container_digests(memo: &mut ContainerDigests) {
    if memo.arrays.len() + memo.objects.len() > memo.prune_at {
        memo.arrays.retain(|_, (held, _)| held.strong_count() > 0);
        memo.objects.retain(|_, (held, _)| held.strong_count() > 0);
        memo.prune_at = ((memo.arrays.len() + memo.objects.len()) * 2).max(4096);
    }
}

/// The xxh3-128 of a wide array's canonical encoding, digested once per allocation: a value is written only through
/// `Shared`'s copy-on-write, which moves it to a new allocation while this memo's weak handle exists, so a held
/// address always names the content that was digested. A table that grows by one row digests the new row only.
fn array_digest(items: &Shared<Vec<EvalValue>>) -> u128 {
    let held: &Vec<EvalValue> = items;
    let at = held as *const Vec<EvalValue> as usize;
    if let Some((weak, digest)) = CONTAINER_DIGESTS.lock().arrays.get(&at) {
        if weak.upgrade().is_some_and(|live| (&*live as *const Vec<EvalValue> as usize) == at) {
            return *digest;
        }
    }
    let mut h = Xxh3::new();
    items.iter().for_each(|v| digest_value(&mut h, v));
    let digest = h.digest128();
    #[cfg(test)]
    CONTAINER_DIGESTS_COMPUTED.with(|count| count.set(count.get() + 1));
    let mut memo = CONTAINER_DIGESTS.lock();
    memo.arrays.insert(at, (Shared::downgrade(items), digest));
    prune_container_digests(&mut memo);
    digest
}

/// [`array_digest`] for a wide object: its keys sorted, each with its value.
fn object_digest(map: &Shared<orbital_core_base::value::ValueMap>) -> u128 {
    let held: &orbital_core_base::value::ValueMap = map;
    let at = held as *const orbital_core_base::value::ValueMap as usize;
    if let Some((weak, digest)) = CONTAINER_DIGESTS.lock().objects.get(&at) {
        if weak.upgrade().is_some_and(|live| (&*live as *const orbital_core_base::value::ValueMap as usize) == at) {
            return *digest;
        }
    }
    let mut h = Xxh3::new();
    let mut keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
    keys.sort();
    for k in keys {
        digest_str(&mut h, k);
        digest_value(&mut h, &map[k]);
    }
    let digest = h.digest128();
    #[cfg(test)]
    CONTAINER_DIGESTS_COMPUTED.with(|count| count.set(count.get() + 1));
    let mut memo = CONTAINER_DIGESTS.lock();
    memo.objects.insert(at, (Shared::downgrade(map), digest));
    prune_container_digests(&mut memo);
    digest
}

/// A bytes value folds in its length and its own xxh3-128 ([`bytes_digest`]); a wide array or object folds in its
/// length and its own ([`array_digest`], [`object_digest`]).
fn digest_value(h: &mut Xxh3, value: &EvalValue) {
    match value {
        EvalValue::Lambda(_) | EvalValue::Sealed(_) => {
            digest_json(h, &serde_json::to_value(value).unwrap_or(Value::Null))
        }
        EvalValue::Null => h.update(b"n"),
        EvalValue::Bool(b) => h.update(if *b { b"t" } else { b"f" }),
        EvalValue::Number(n) if n.is_finite() => digest_float(h, *n),
        EvalValue::Number(_) => h.update(b"n"),
        EvalValue::Int(i) => digest_integer(h, *i),
        EvalValue::Bytes(b) => {
            h.update(b"b");
            digest_len(h, b.len());
            h.update(&bytes_digest(b).to_le_bytes());
        }
        EvalValue::String(s) => {
            h.update(b"s");
            digest_str(h, s);
        }
        EvalValue::Array(items) if items.len() >= CONTAINER_MEMO_MIN => {
            h.update(b"A");
            digest_len(h, items.len());
            h.update(&array_digest(items).to_le_bytes());
        }
        EvalValue::Array(items) => {
            h.update(b"a");
            digest_len(h, items.len());
            items.iter().for_each(|v| digest_value(h, v));
        }
        EvalValue::Object(map) if map.len() >= CONTAINER_MEMO_MIN => {
            h.update(b"O");
            digest_len(h, map.len());
            h.update(&object_digest(map).to_le_bytes());
        }
        EvalValue::Object(map) => {
            h.update(b"o");
            digest_len(h, map.len());
            let mut keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
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
            let mut keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
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
                    EValue::Object(o.into_iter().map(|(k, v)| (k.into(), conv(v))).collect())
                }
            }
        }
        conv(v)
    }

    fn lambda(param: &str) -> EValue {
        EValue::lambda(vec![param.to_string()], Arc::new(SExpression::string(param.to_string())))
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
        let held = || EValue::lambda(vec!["x".to_string()], Arc::clone(&body));
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
    fn two_traits_with_one_field_name_keep_a_digest_each() {
        let mut visits = DispatchVisits::default();
        let first = Arc::new(ev(json!({ "rows": [1, 2, 3] })));
        let second = Arc::new(ev(json!({ "rows": [4, 5, 6] })));
        for _ in 0..2 {
            visits.insert("A", "E", "s", &EValue::Null, &fields(&[("program", Arc::clone(&first))]));
            visits.insert("B", "E", "s", &EValue::Null, &fields(&[("program", Arc::clone(&second))]));
        }
        assert_eq!(Arc::strong_count(&first), 2, "A's field digest is still held after B stepped");
        assert_eq!(Arc::strong_count(&second), 2, "B's field digest is still held after A stepped");
        assert!(visits.contains("A", "E", "s", &EValue::Null, &fields(&[("program", Arc::clone(&first))])));
        assert!(!visits.contains("A", "E", "s", &EValue::Null, &fields(&[("program", Arc::clone(&second))])), "control: the identity still reads the value");
    }

    fn wide(seed: i64) -> EValue {
        EValue::Array((0..CONTAINER_MEMO_MIN as i64).map(|i| EValue::Int(seed + i)).collect::<Vec<_>>().into())
    }

    #[test]
    fn a_wide_container_is_one_value_by_content_whatever_holds_it() {
        let mut visits = DispatchVisits::default();
        assert!(visits.insert("T", "E", "s", &EValue::Null, &frame(&wide(1))));
        assert!(visits.contains("T", "E", "s", &EValue::Null, &frame(&wide(1))), "an equal wide array in a new allocation");
        assert!(!visits.contains("T", "E", "s", &EValue::Null, &frame(&wide(2))), "control: another wide array is another frame");
        let narrow = |n: i64| ev(json!([n, n + 1]));
        assert!(visits.insert("T", "E", "s", &EValue::Null, &frame(&narrow(1))));
        assert!(!visits.contains("T", "E", "s", &EValue::Null, &frame(&wide(1 << 20))), "control: a narrow array is not a wide one");
    }

    #[test]
    fn a_wide_container_written_in_place_is_digested_again() {
        let mut items: Shared<Vec<EValue>> = (0..CONTAINER_MEMO_MIN as i64).map(EValue::Int).collect::<Vec<_>>().into();
        let before = payload_digest(&EValue::Array(items.clone()));
        items.push(EValue::Int(-1));
        let after = payload_digest(&EValue::Array(items.clone()));
        assert_ne!(before, after, "the only handle wrote through copy-on-write: the memo does not answer for the old content");
        items.pop();
        assert_eq!(payload_digest(&EValue::Array(items)), before, "control: back to the first content is the first digest");
    }

    #[test]
    fn a_grown_container_digests_only_what_is_new() {
        let rows: Vec<EValue> = (0..64).map(|i| wide(i * 100)).collect();
        let held = EValue::Array(rows.clone().into());
        let before = container_digests_computed();
        payload_digest(&held);
        let first = container_digests_computed() - before;
        assert_eq!(first, 65, "the table and each of its 64 wide rows, once");
        let mut grown = rows.clone();
        grown.push(wide(999_000));
        let grown = EValue::Array(grown.into());
        let before = container_digests_computed();
        let a = payload_digest(&grown);
        assert_eq!(container_digests_computed() - before, 2, "the new table and its one new row: the 64 held rows keep their digests");
        let before = container_digests_computed();
        assert_eq!(payload_digest(&grown), a);
        assert_eq!(container_digests_computed() - before, 0, "control: the same allocation again digests nothing");
        let rebuilt: Vec<EValue> = (0..64).map(|i| wide(i * 100)).chain([wide(999_000)]).collect();
        assert_eq!(payload_digest(&EValue::Array(rebuilt.into())), a, "control: equal content in new allocations is the same digest");
    }

    #[test]
    fn a_field_of_byte_chunks_is_one_frame_by_content_and_a_new_chunk_is_a_new_frame() {
        let chunk = |b: &[u8]| EValue::bytes(b.to_vec());
        let parts = |items: Vec<EValue>| Arc::new(EValue::Array(items.into()));
        let mut visits = DispatchVisits::default();
        let a = chunk(b"alpha");
        let b = chunk(b"beta");
        let grown = parts(vec![a.clone(), b.clone()]);
        assert!(visits.insert("T", "E", "s", &EValue::Null, &fields(&[("parts", parts(vec![a.clone()]))])));
        assert!(visits.insert("T", "E", "s", &EValue::Null, &fields(&[("parts", Arc::clone(&grown))])), "a grown list is a new frame");
        let copied = parts(vec![chunk(b"alpha"), chunk(b"beta")]);
        assert!(
            visits.contains("T", "E", "s", &EValue::Null, &fields(&[("parts", copied)])),
            "equal chunks in new allocations are the same frame"
        );
        let other = parts(vec![chunk(b"alpha"), chunk(b"betA")]);
        assert!(!visits.contains("T", "E", "s", &EValue::Null, &fields(&[("parts", other)])), "control: one changed byte is another frame");
        assert!(
            !visits.contains("T", "E", "s", &EValue::Null, &fields(&[("parts", Arc::new(chunk(b"alphabeta")))])),
            "control: the joined bytes are not the list of chunks"
        );
    }

    #[test]
    fn a_large_bytes_value_is_digested_by_content_across_allocations_and_dispatches() {
        let big = |fill: u8| EValue::bytes(vec![fill; BYTES_MEMO_MIN * 4]);
        let first = big(7);
        let mut one = DispatchVisits::default();
        assert!(one.insert("T", "E", "s", &first, &frame(&EValue::Null)));
        let mut next = DispatchVisits::default();
        assert!(next.insert("T", "E", "s", &first, &frame(&EValue::Null)), "a new dispatch keeps its own visits");
        assert!(next.contains("T", "E", "s", &big(7), &frame(&EValue::Null)), "an equal copy in another allocation is the same payload");
        drop(first);
        for _ in 0..64 {
            assert!(
                !next.contains("T", "E", "s", &big(8), &frame(&EValue::Null)),
                "edge: a freed allocation's address reused by other bytes is never taken for them"
            );
        }
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
