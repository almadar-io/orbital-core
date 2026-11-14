//! Held values (EXEC-DIST-16): a browser kernel's answer names a render value it already handed that host by
//! reference. Inside a `render-ui` payload, a list or record the kernel also holds elsewhere is written once as
//! `{ "$held": n, "value": … }` and as `{ "$held": n }` after that; the host keeps `n` until it is released.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use crate::value::{Shared, Value, ValueMap};
use core::cell::RefCell;
use serde::ser::{SerializeMap, Serializer};
use std::collections::BTreeMap;
use std::sync::Weak;

/// Key of a held-value leaf (core's `HELD_MARKER`).
pub const HELD_MARKER: &str = "$held";

#[derive(Clone)]
enum HeldWeak {
    Array(Weak<Vec<Value>>),
    Object(Weak<ValueMap>),
}

impl HeldWeak {
    fn alive(&self) -> bool {
        match self {
            HeldWeak::Array(w) => w.strong_count() > 0,
            HeldWeak::Object(w) => w.strong_count() > 0,
        }
    }
}

/// What one host has been handed: each held allocation's id, and the ids freed since the host last asked.
#[derive(Default)]
pub struct HeldValues {
    sent: BTreeMap<usize, (u64, HeldWeak)>,
    next: u64,
    released: Vec<u64>,
}

impl HeldValues {
    pub fn new() -> Self {
        Self::default()
    }

    /// The ids whose values the kernel no longer holds, oldest first; the host drops them.
    pub fn take_released(&mut self) -> Vec<u64> {
        core::mem::take(&mut self.released)
    }

    /// How many values the host holds for this kernel.
    pub fn held(&self) -> usize {
        self.sent.len()
    }

    fn sweep(&mut self) {
        let dead: Vec<usize> = self.sent.iter().filter(|(_, (_, w))| !w.alive()).map(|(at, _)| *at).collect();
        for at in dead {
            if let Some((id, _)) = self.sent.remove(&at) {
                self.released.push(id);
            }
        }
    }
}

struct Scope {
    held: HeldValues,
    pending: Vec<(usize, u64, HeldWeak)>,
    writing: bool,
    render_depth: u32,
}

impl Scope {
    fn id_of(&self, at: usize) -> Option<u64> {
        self.held
            .sent
            .get(&at)
            .filter(|(_, w)| w.alive())
            .map(|(id, _)| *id)
            .or_else(|| if self.writing { self.pending.iter().find(|(p, _, _)| *p == at).map(|(_, id, _)| *id) } else { None })
    }
}

crate::kernel_local! {
    static SCOPE: RefCell<Option<Scope>> = const { RefCell::new(None) };
}

/// `value` as JSON text a JavaScript host reads exactly, naming each held render value `held` records as handed
/// over by reference. Nothing is recorded when the text cannot be written.
pub fn to_js_readable_json_held<T: serde::Serialize + ?Sized>(value: &T, held: &mut HeldValues) -> Result<String, String> {
    held.sweep();
    let next = held.next;
    let open = Scope { held: core::mem::take(held), pending: Vec::new(), writing: false, render_depth: 0 };
    SCOPE.with(|s| *s.borrow_mut() = Some(open));
    let checked = crate::js_readable::js_readable(value).map_err(|e| e.to_string());
    let written = checked.and_then(|()| {
        SCOPE.with(|s| if let Some(scope) = s.borrow_mut().as_mut() { scope.writing = true });
        serde_json::to_string(value).map_err(|e| format!("serialize: {e}"))
    });
    let scope = SCOPE.with(|s| s.borrow_mut().take());
    let Some(mut scope) = scope else { return Err("held-value scope lost".to_string()) };
    match &written {
        Ok(_) => {
            for (at, id, weak) in scope.pending.drain(..) {
                scope.held.sent.insert(at, (id, weak));
            }
        }
        Err(_) => scope.held.next = next,
    }
    *held = scope.held;
    written
}

/// Runs `f` (a `render-ui` payload's serialization) with held values written by reference when a scope is open.
pub(crate) fn in_render<R>(f: impl FnOnce() -> R) -> R {
    let entered = SCOPE.with(|s| s.borrow_mut().as_mut().map(|scope| scope.render_depth += 1).is_some());
    let out = f();
    if entered {
        SCOPE.with(|s| if let Some(scope) = s.borrow_mut().as_mut() { scope.render_depth -= 1 });
    }
    out
}

pub(crate) enum Held<S: Serializer> {
    Done(Result<S::Ok, S::Error>),
    Inline(S),
}

enum Decision {
    Inline,
    Skip,
    Reference(u64),
    First(u64),
}

/// `value` written by reference when a scope is open, it sits in a `render-ui` payload and another holder shares it.
pub(crate) fn write<S: Serializer>(value: &Value, serializer: S) -> Held<S> {
    let (at, weak, shared) = match value {
        Value::Array(a) => (Shared::as_ptr(a) as usize, HeldWeak::Array(Shared::downgrade(a)), Shared::is_shared(a)),
        Value::Object(o) => (Shared::as_ptr(o) as usize, HeldWeak::Object(Shared::downgrade(o)), Shared::is_shared(o)),
        _ => return Held::Inline(serializer),
    };
    let decision = SCOPE.with(|s| {
        let mut slot = s.borrow_mut();
        let Some(scope) = slot.as_mut() else { return Decision::Inline };
        if scope.render_depth == 0 || !shared {
            return Decision::Inline;
        }
        match (scope.id_of(at), scope.writing) {
            (Some(_), false) => Decision::Skip,
            (Some(id), true) => Decision::Reference(id),
            (None, false) => Decision::Inline,
            (None, true) => {
                let id = scope.held.next;
                scope.held.next += 1;
                scope.pending.push((at, id, weak));
                Decision::First(id)
            }
        }
    });
    match decision {
        Decision::Inline => Held::Inline(serializer),
        Decision::Skip => Held::Done(serializer.serialize_unit()),
        Decision::Reference(id) => Held::Done((|| {
            let mut map = serializer.serialize_map(Some(1))?;
            map.serialize_entry(HELD_MARKER, &id)?;
            map.end()
        })()),
        Decision::First(id) => Held::Done((|| {
            let mut map = serializer.serialize_map(Some(2))?;
            map.serialize_entry(HELD_MARKER, &id)?;
            match value {
                Value::Array(a) => map.serialize_entry("value", &**a)?,
                Value::Object(o) => map.serialize_entry("value", &**o)?,
                _ => {}
            }
            map.end()
        })()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_effect::ClientEffect;
    use crate::js_readable::to_js_readable_json;
    use serde_json::json;

    fn program() -> Value {
        let mut m = ValueMap::default();
        m.insert("name".into(), Value::String(Shared::new("crm".to_string())));
        m.insert("traits".into(), Value::Array(Shared::new(vec![Value::Int(1), Value::Int(2)])));
        Value::Object(Shared::new(m))
    }

    fn render(program: Value) -> ClientEffect {
        let mut view = ValueMap::default();
        view.insert("type".into(), Value::String(Shared::new("guest-view".to_string())));
        view.insert("program".into(), program);
        ClientEffect::RenderUi { slot: "main".to_string(), config: Some(Value::Object(Shared::new(view))) }
    }

    fn resolve(v: serde_json::Value, cache: &mut BTreeMap<u64, serde_json::Value>) -> serde_json::Value {
        match v {
            serde_json::Value::Object(m) if m.contains_key(HELD_MARKER) => {
                let id = m[HELD_MARKER].as_u64().expect("id");
                match m.get("value") {
                    Some(inner) => {
                        let full = resolve(inner.clone(), cache);
                        cache.insert(id, full.clone());
                        full
                    }
                    None => cache.get(&id).cloned().expect("held id the host was handed"),
                }
            }
            serde_json::Value::Object(m) => serde_json::Value::Object(m.into_iter().map(|(k, v)| (k, resolve(v, cache))).collect()),
            serde_json::Value::Array(a) => serde_json::Value::Array(a.into_iter().map(|v| resolve(v, cache)).collect()),
            other => other,
        }
    }

    fn text(s: &str) -> serde_json::Value {
        serde_json::from_str(s).expect("json")
    }

    #[test]
    fn a_shared_render_value_is_sent_once_then_by_reference() {
        let row = program();
        let mut held = HeldValues::new();
        let first = to_js_readable_json_held(&render(row.clone()), &mut held).expect("first");
        assert_eq!(text(&first)[2]["program"], json!({ "$held": 0, "value": { "name": "crm", "traits": [1, 2] } }));
        let second = to_js_readable_json_held(&render(row.clone()), &mut held).expect("second");
        assert_eq!(text(&second), json!(["render-ui", "main", { "type": "guest-view", "program": { "$held": 0 } }]));
        assert_eq!(held.held(), 1);
    }

    #[test]
    fn resolving_the_references_gives_the_plain_text() {
        let row = program();
        let mut held = HeldValues::new();
        let mut cache = BTreeMap::new();
        for _ in 0..3 {
            let plain = text(&to_js_readable_json(&render(row.clone())).expect("plain"));
            let by_ref = text(&to_js_readable_json_held(&render(row.clone()), &mut held).expect("held"));
            assert_eq!(resolve(by_ref, &mut cache), plain);
        }
    }

    #[test]
    fn a_value_nothing_else_holds_is_written_inline() {
        let mut held = HeldValues::new();
        let out = to_js_readable_json_held(&render(program()), &mut held).expect("inline");
        assert_eq!(text(&out)[2]["program"], json!({ "name": "crm", "traits": [1, 2] }));
        assert_eq!(held.held(), 0);
    }

    #[test]
    fn the_plain_encoder_never_writes_a_reference() {
        let row = program();
        let out = to_js_readable_json(&render(row.clone())).expect("plain");
        assert_eq!(text(&out)[2]["program"], json!({ "name": "crm", "traits": [1, 2] }));
    }

    #[test]
    fn a_shared_value_outside_a_render_is_written_inline() {
        let row = program();
        let effect = ClientEffect::Navigate { path: "/x".to_string(), params: Some(row.clone()), options: None };
        let mut held = HeldValues::new();
        let out = to_js_readable_json_held(&effect, &mut held).expect("navigate");
        assert_eq!(text(&out)[2], json!({ "name": "crm", "traits": [1, 2] }));
        assert_eq!(held.held(), 0);
    }

    #[test]
    fn the_same_value_twice_in_one_answer_is_sent_once() {
        let row = program();
        let mut held = HeldValues::new();
        let out = to_js_readable_json_held(&vec![render(row.clone()), render(row.clone())], &mut held).expect("two");
        let out = text(&out);
        assert_eq!(out[0][2]["program"]["$held"], json!(0));
        assert!(out[0][2]["program"].get("value").is_some());
        assert_eq!(out[1][2]["program"], json!({ "$held": 0 }));
    }

    #[test]
    fn a_value_the_kernel_frees_is_released_and_never_referenced_again() {
        let mut held = HeldValues::new();
        let row = program();
        to_js_readable_json_held(&render(row.clone()), &mut held).expect("first");
        drop(row);
        let other = program();
        let out = to_js_readable_json_held(&render(other.clone()), &mut held).expect("after free");
        assert_eq!(held.take_released(), vec![0]);
        assert_eq!(text(&out)[2]["program"]["$held"], json!(1));
        assert!(text(&out)[2]["program"].get("value").is_some());
        assert!(held.take_released().is_empty());
    }

    #[test]
    fn an_answer_that_cannot_be_written_records_nothing() {
        let row = program();
        let mut held = HeldValues::new();
        let too_big = ClientEffect::Navigate { path: "/x".to_string(), params: Some(Value::Int(i64::MAX)), options: None };
        let failing = (render(row.clone()), too_big);
        assert!(to_js_readable_json_held(&failing, &mut held).is_err());
        assert_eq!(held.held(), 0);
        let out = to_js_readable_json_held(&render(row.clone()), &mut held).expect("after failure");
        assert_eq!(text(&out)[2]["program"], json!({ "$held": 0, "value": { "name": "crm", "traits": [1, 2] } }));
    }
}
