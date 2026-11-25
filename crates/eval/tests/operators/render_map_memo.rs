// A render that maps over a list computes only the elements that changed since its last render and answers the
// rest from what the slot kept (EXEC-RENDER-9). Every case checks the result against the same render with nothing kept.

use orbital_core_base::value::{value_map_with_capacity, Value};
use orbital_core_eval::evaluator::context::BindingContextBuilder;
use orbital_core_eval::evaluator::map_memo::{MapMemo, SharedMapMemo};
use serde_json::{json, Value as Json};
use std::collections::HashMap;

fn value(json: Json) -> Value {
    serde_json::from_value(json).unwrap()
}

fn row(name: &str, n: i64) -> Value {
    value(json!({ "name": name, "n": n }))
}

fn entity(items: Vec<Value>, bias: i64, other: i64) -> Value {
    let mut fields = value_map_with_capacity(3);
    fields.insert("items".to_owned().into(), Value::array(items));
    fields.insert("bias".to_owned().into(), value(json!(bias)));
    fields.insert("other".to_owned().into(), value(json!(other)));
    Value::object(fields)
}

fn content(lambda: Json) -> Value {
    value(json!({ "type": "list", "rows": ["array/map", "@entity.items", lambda] }))
}

fn labelled() -> Json {
    json!(["fn", "it", { "label": ["str/concat", "@it.name", "!"], "sum": ["+", "@it.n", "@entity.bias"] }])
}

/// One slot's renders: each is checked against a render that keeps nothing.
struct Slot {
    memo: SharedMapMemo,
    content: Value,
}

impl Slot {
    fn of(content: Value) -> Self {
        Slot { memo: SharedMapMemo::default(), content }
    }

    /// The content rendered over `entity`: what it resolves to, and how many mapped elements it reused and computed.
    fn render(&self, entity: &Value) -> (Json, usize, usize) {
        self.render_in(BindingContextBuilder::new().entity(entity.clone()))
    }

    fn render_in(&self, builder: BindingContextBuilder) -> (Json, usize, usize) {
        let plain = builder.build();
        let mut kept = plain.clone();
        kept.set_map_memo(self.memo.clone());
        let (reused, computed) = {
            let mut memo = self.memo.lock();
            memo.begin();
            memo.counts()
        };
        let out = serde_json::to_value(kept.resolve_value_checked(&self.content).unwrap().into_owned()).unwrap();
        let (reused_after, computed_after) = self.memo.lock().counts();
        let fresh = serde_json::to_value(plain.resolve_value_checked(&self.content).unwrap().into_owned()).unwrap();
        assert_eq!(out, fresh, "a render that reuses answers what a render that keeps nothing answers");
        (out, reused_after - reused, computed_after - computed)
    }
}

#[test]
fn a_render_computes_only_the_mapped_elements_that_changed() {
    let slot = Slot::of(content(labelled()));
    let (a, b, c) = (row("a", 1), row("b", 2), row("c", 3));
    let first = entity(vec![a.clone(), b.clone(), c.clone()], 10, 0);
    let (out, reused, computed) = slot.render(&first);
    assert_eq!(out["rows"][1]["label"], json!("b!"));
    assert_eq!((reused, computed), (0, 3), "control: a first render computes every element");
    let (_, reused, computed) = slot.render(&first);
    assert_eq!((reused, computed), (3, 0), "nothing changed: every element is the one kept");
    let (out, reused, computed) = slot.render(&entity(vec![a.clone(), row("b2", 20), c.clone()], 10, 0));
    assert_eq!((reused, computed), (2, 1), "one element is new: only it is computed");
    assert_eq!(out["rows"][1]["label"], json!("b2!"));
    let (out, reused, computed) = slot.render(&entity(vec![c.clone(), a.clone()], 10, 0));
    assert_eq!((reused, computed), (2, 0), "a kept element answers wherever it sits when the callback does not name its place");
    assert_eq!(out["rows"][0]["label"], json!("c!"));
    let (_, reused, computed) = slot.render(&entity(vec![row("a", 1)], 10, 0));
    assert_eq!((reused, computed), (0, 1), "an equal element in another allocation is another element");
}

#[test]
fn a_binding_the_callback_reads_changes_every_element_and_one_it_does_not_read_changes_none() {
    let slot = Slot::of(content(labelled()));
    let items = vec![row("a", 1), row("b", 2)];
    slot.render(&entity(items.clone(), 10, 0));
    let (out, reused, computed) = slot.render(&entity(items.clone(), 11, 0));
    assert_eq!((reused, computed), (0, 2), "`@entity.bias` is read by the callback: nothing kept answers for it");
    assert_eq!(out["rows"][0]["sum"], json!(12));
    let (_, reused, computed) = slot.render(&entity(items.clone(), 11, 5));
    assert_eq!((reused, computed), (2, 0), "control: `@entity.other` is not read, so its change costs nothing");
}

#[test]
fn a_callback_that_names_the_index_is_kept_by_place() {
    let slot = Slot::of(content(json!(["fn", ["it", "i"], { "at": "@i", "name": "@it.name" }])));
    let (a, b) = (row("a", 1), row("b", 2));
    slot.render(&entity(vec![a.clone(), b.clone()], 0, 0));
    let (out, reused, computed) = slot.render(&entity(vec![b.clone(), a.clone()], 0, 0));
    assert_eq!((reused, computed), (0, 2), "an element at another place is computed");
    assert_eq!(out["rows"][0], json!({ "at": 0, "name": "b" }));
    let (_, reused, computed) = slot.render(&entity(vec![b.clone(), a.clone()], 0, 0));
    assert_eq!((reused, computed), (2, 0), "control: at the same places they are kept");
}

#[test]
fn a_callback_with_an_operator_that_is_not_pure_is_computed_every_render() {
    let (a, b) = (row("a", 1), row("b", 2));
    let items = entity(vec![a, b], 0, 0);
    for impure in [json!(["time/isPast", "2000-01-01T00:00:00Z"]), json!(["time/year", "2000-01-01T00:00:00Z"])] {
        let slot = Slot::of(value(json!({ "rows": ["array/len", ["array/map", "@entity.items", ["fn", "it", { "v": impure, "name": "@it.name" }]]] })));
        slot.render(&items);
        let (out, reused, computed) = slot.render(&items);
        assert_eq!(out["rows"], json!(2));
        assert_eq!((reused, computed), (0, 0), "{impure}: nothing is kept and nothing is counted");
    }
    let slot = Slot::of(value(json!({ "rows": ["array/len", ["array/map", "@entity.items", ["fn", "it", { "v": ["math/max", 1, "@it.n"] }]]] })));
    slot.render(&items);
    assert_eq!(slot.render(&items).1, 2, "control: the same shape with a pure operator is kept");
}

#[test]
fn a_lambda_value_applied_inside_a_callback_keeps_its_result_out() {
    let through = |callback: Json| {
        value(json!(["let", [["f", ["fn", "x", ["time/isPast", "2000-01-01T00:00:00Z"]]]], ["array/len", ["array/map", "@entity.items", ["fn", "it", { "rolls": ["array/map", ["list", 1], callback] }]]]]))
    };
    let items = entity(vec![row("a", 1), row("b", 2)], 0, 0);
    let slot = Slot::of(through(json!("@f")));
    slot.render(&items);
    let (_, reused, computed) = slot.render(&items);
    assert_eq!((reused, computed), (0, 2), "the lambda `@f` holds is not the body read at the site: its rows are computed again");
    let slot = Slot::of(through(json!(["fn", "x", ["+", "@x", 1]])));
    slot.render(&items);
    assert_eq!(slot.render(&items).1, 2, "control: with a callback written in place the rows are kept");
}

#[test]
fn a_translated_callback_is_kept_under_one_locale_and_computed_under_another() {
    let slot = Slot::of(content(json!(["fn", "it", { "said": ["i18n/t", "@it.name"] }])));
    let items = entity(vec![row("hello", 1), row("bye", 2)], 0, 0);
    let catalog = |hello: &str, bye: &str| HashMap::from([("hello".to_string(), hello.to_string()), ("bye".to_string(), bye.to_string())]);
    let under = |locale: &str, messages: HashMap<String, String>| {
        BindingContextBuilder::new().entity(items.clone()).locale(value(json!(locale))).messages(messages)
    };
    let english = under("en", catalog("Hello", "Bye")).build();
    let again = |slot: &Slot| {
        let mut kept = english.clone();
        kept.set_map_memo(slot.memo.clone());
        let before = slot.memo.lock().counts();
        slot.memo.lock().begin();
        let out = serde_json::to_value(kept.resolve_value_checked(&slot.content).unwrap().into_owned()).unwrap();
        let after = slot.memo.lock().counts();
        (out, after.0 - before.0, after.1 - before.1)
    };
    let (out, reused, computed) = again(&slot);
    assert_eq!(out["rows"][0]["said"], json!("Hello"));
    assert_eq!((reused, computed), (0, 2));
    let (_, reused, computed) = again(&slot);
    assert_eq!((reused, computed), (2, 0), "control: the same locale and catalog keep the translated rows");
    let (out, reused, computed) = slot.render_in(under("sl", catalog("Živjo", "Adijo")));
    assert_eq!((reused, computed), (0, 2), "another locale: nothing kept answers");
    assert_eq!(out["rows"][0]["said"], json!("Živjo"));
}

#[test]
fn a_nested_map_keeps_its_elements_and_an_evaluation_with_no_memo_keeps_nothing() {
    let nested = content(json!(["fn", "it", { "name": "@it.name", "tags": ["array/map", "@it.tags", ["fn", "t", { "tag": "@t.k" }]] }]));
    let tagged = |name: &str, tags: Value| {
        let mut fields = value_map_with_capacity(2);
        fields.insert("name".to_owned().into(), value(json!(name)));
        fields.insert("tags".to_owned().into(), tags);
        Value::object(fields)
    };
    let (x, y, z) = (value(json!({ "k": "x" })), value(json!({ "k": "y" })), value(json!({ "k": "z" })));
    let a = tagged("a", Value::array(vec![x.clone(), y.clone()]));
    let b = tagged("b", Value::array(vec![z.clone()]));
    let slot = Slot::of(nested.clone());
    let (_, reused, computed) = slot.render(&entity(vec![a.clone(), b.clone()], 0, 0));
    assert_eq!((reused, computed), (0, 5), "control: two rows and their three tags");
    let renamed = tagged("b2", Value::array(vec![z.clone(), value(json!({ "k": "w" }))]));
    let (out, reused, computed) = slot.render(&entity(vec![a.clone(), renamed], 0, 0));
    assert_eq!((reused, computed), (2, 2), "row a and tag z are kept; the changed row and its new tag are computed");
    assert_eq!(out["rows"][1]["tags"][1]["tag"], json!("w"));
    let memo = MapMemo::default();
    assert_eq!(memo.counts(), (0, 0));
    let plain = BindingContextBuilder::new().entity(entity(vec![a.clone()], 0, 0)).build();
    assert_eq!(
        serde_json::to_value(plain.resolve_value_checked(&nested).unwrap().into_owned()).unwrap()["rows"][0]["tags"][1]["tag"],
        json!("y"),
        "control: with no memo the render resolves as before"
    );
}

#[test]
fn a_predicate_is_kept_per_element_and_one_that_stops_early_computes_no_further() {
    let (a, b, c) = (row("a", 1), row("b", 2), row("c", 3));
    let items = entity(vec![a.clone(), b.clone(), c.clone()], 0, 0);
    let big = json!(["fn", "it", [">", "@it.n", 1]]);
    let filtered = Slot::of(value(json!({ "rows": ["array/filter", "@entity.items", big.clone()] })));
    let (out, reused, computed) = filtered.render(&items);
    assert_eq!(out["rows"], json!([{ "name": "b", "n": 2 }, { "name": "c", "n": 3 }]));
    assert_eq!((reused, computed), (0, 3), "control: a first render asks the predicate of every element");
    let (_, reused, computed) = filtered.render(&entity(vec![a.clone(), row("d", 9), c.clone()], 0, 0));
    assert_eq!((reused, computed), (2, 1), "only the new element is asked again");

    let found = Slot::of(value(json!({ "row": ["array/find", "@entity.items", big.clone()] })));
    let (out, reused, computed) = found.render(&items);
    assert_eq!(out["row"]["name"], json!("b"));
    assert_eq!((reused, computed), (0, 2), "find stops at the first element it accepts, as it does with nothing kept");
    let (_, reused, computed) = found.render(&items);
    assert_eq!((reused, computed), (2, 0));

    let any = Slot::of(value(json!({ "some": ["array/some", "@entity.items", big.clone()], "every": ["array/every", "@entity.items", big] })));
    let (out, _, computed) = any.render(&items);
    assert_eq!(out, json!({ "some": true, "every": false }));
    assert_eq!(computed, 3, "some stops at b and every stops at a");
    let (_, reused, computed) = any.render(&items);
    assert_eq!((reused, computed), (3, 0));
}
