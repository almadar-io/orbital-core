// `array/pmap` and `array/preduce` answer what their sequential twins answer; `array/preduce` groups by a fixed chunk
// of 1024, so a non-associative fold has one answer however many threads ran it.

use {orbital_core_base::error::OrbitalError, orbital_core_eval::evaluator::context::BindingContextBuilder, orbital_core_eval::evaluator::Evaluator};
use serde_json::{json, Value as Json};

fn run(expr: Json) -> Result<Json, OrbitalError> {
    let ctx = BindingContextBuilder::new().build();
    let sexpr = serde_json::from_value(expr).unwrap();
    Evaluator::new().evaluate(&sexpr, &ctx).map(|value| serde_json::to_value(value).unwrap())
}

fn numbers(value: &Json) -> Vec<f64> {
    value.as_array().unwrap().iter().map(|n| n.as_f64().unwrap()).collect()
}

fn ints(count: i64) -> Json {
    Json::Array((0..count).map(|n| json!(n)).collect())
}

#[test]
fn pmap_equals_map_in_input_order() {
    let items = ints(3000);
    let lambda = json!(["fn", ["x", "i"], ["+", ["*", "@x", 2], "@i"]]);
    let parallel = run(json!(["array/pmap", items.clone(), lambda.clone()])).unwrap();
    let sequential = run(json!(["array/map", items, lambda])).unwrap();
    assert_eq!(numbers(&parallel), numbers(&sequential));
    assert_eq!(numbers(&parallel)[2999], 2999.0 * 3.0);
}

#[test]
fn pmap_of_an_empty_array_and_a_non_function_edge() {
    assert_eq!(run(json!(["array/pmap", [], ["fn", "x", "@x"]])).unwrap(), json!([]));
    assert!(run(json!(["array/pmap", [1, 2], null])).is_err(), "control: the callback must be a function");
}

#[test]
fn preduce_equals_reduce_for_integer_addition() {
    let items = ints(3000);
    let add = json!(["fn", ["a", "x"], ["+", "@a", "@x"]]);
    let parallel = run(json!(["array/preduce", items.clone(), 7, add.clone()])).unwrap();
    let sequential = run(json!(["array/reduce", items, 7, add])).unwrap();
    assert_eq!(parallel.as_f64(), sequential.as_f64());
    assert_eq!(parallel.as_f64(), Some(7.0 + 2999.0 * 3000.0 / 2.0));
}

#[test]
fn preduce_groups_by_fixed_chunks_for_a_fold_that_is_not_associative() {
    let items: Vec<i64> = (0..3000).collect();
    let chunk_results: Vec<i64> = items.chunks(1024).map(|chunk| chunk[1..].iter().fold(chunk[0], |acc, x| acc - x)).collect();
    let expected = chunk_results.iter().fold(100, |acc, chunk| acc - chunk);
    let got = run(json!(["array/preduce", ints(3000), 100, ["fn", ["a", "x"], ["-", "@a", "@x"]]])).unwrap();
    assert_eq!(got.as_f64(), Some(expected as f64));
    let sequential = run(json!(["array/reduce", ints(3000), 100, ["fn", ["a", "x"], ["-", "@a", "@x"]]])).unwrap();
    assert_ne!(got.as_f64(), sequential.as_f64(), "control: the fixed grouping differs from a plain left fold");
}

#[test]
fn preduce_of_an_empty_array_answers_init() {
    assert_eq!(run(json!(["array/preduce", [], 41, ["fn", ["a", "x"], ["+", "@a", "@x"]]])).unwrap().as_f64(), Some(41.0));
    assert_eq!(run(json!(["array/preduce", [5], 1, ["fn", ["a", "x"], ["+", "@a", "@x"]]])).unwrap().as_f64(), Some(6.0), "edge: one element");
}

#[test]
fn the_first_error_in_index_order_is_the_one_returned() {
    let mut items: Vec<Json> = (0..3000).map(|n| json!(n)).collect();
    items[5] = json!("text");
    items[2000] = json!({});
    let call = json!(["array/pmap", items.clone(), ["fn", "x", ["math/abs", "@x"]]]);
    let first = run(call.clone()).unwrap_err().to_string();
    assert!(first.contains("string"), "{first}");
    for _ in 0..20 {
        assert_eq!(run(call.clone()).unwrap_err().to_string(), first);
    }
    let reduce = json!(["array/preduce", items, 0, ["fn", ["a", "x"], ["+", "@a", ["math/abs", "@x"]]]]);
    let reduced = run(reduce.clone()).unwrap_err().to_string();
    assert!(reduced.contains("string"), "{reduced}");
    for _ in 0..20 {
        assert_eq!(run(reduce.clone()).unwrap_err().to_string(), reduced);
    }
}
