// A fold accumulator read field by field: the last read of a field, in evaluation order, moves it out, so a queue or a
// map threaded through `array/reduce` grows in place. Results are what a copying fold answers (EXEC-PERF-1).

use orbital_core_base::alloc_meter;
use orbital_core_base::json_value::from_json_value;
use {orbital_core_eval::evaluator::context::BindingContextBuilder, orbital_core_eval::evaluator::Evaluator};
use serde_json::{json, Value as Json};

fn eval(expr: Json) -> Json {
    let ctx = BindingContextBuilder::new().build();
    let sexpr = from_json_value(&expr).unwrap();
    serde_json::to_value(Evaluator::new().evaluate(&sexpr, &ctx).unwrap()).unwrap()
}

/// `n` steps over `{ q, head, seen }`: append `i` to `q`, count it, record it under `k<i mod 5>`; an empty `step` reads
/// the accumulator whole first, as a worklist's "nothing left" arm does.
fn queue_fold(n: i64, step: Json) -> Json {
    json!(["array/reduce", ["array/range", n], { "q": [], "head": 0, "seen": {} }, ["fn", ["acc", "i"], step]])
}

fn growing_step() -> Json {
    json!(["if", [">", ["array/len", "@acc.q"], 1_000_000], "@acc", {
        "q": ["array/concat", "@acc.q", ["list", "@i"]],
        "head": ["+", "@acc.head", 1],
        "seen": ["object/set", "@acc.seen", ["str/concat", "k", ["math/mod", "@i", 5]], "@i"]
    }])
}

fn ints(json: &Json) -> Vec<i64> {
    json.as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as i64).collect()
}

#[test]
fn a_field_read_last_is_moved_and_the_fold_answers_what_a_copying_fold_does() {
    let got = eval(queue_fold(12, growing_step()));
    assert_eq!(ints(&got["q"]), (0..12).collect::<Vec<i64>>());
    assert_eq!(got["head"].as_f64(), Some(12.0));
    assert_eq!(got["seen"]["k1"].as_f64(), Some(11.0), "{got}");
    assert_eq!(got["seen"].as_object().map(|m| m.len()), Some(5), "{got}");
}

#[test]
fn control_a_read_inside_a_nested_lambda_keeps_every_read_a_copy() {
    let step = json!({
        "q": ["array/concat", "@acc.q", ["array/map", ["list", 1], ["fn", "x", ["array/len", "@acc.q"]]]],
        "head": ["+", "@acc.head", 1],
        "seen": "@acc.seen"
    });
    let got = eval(queue_fold(4, step));
    assert_eq!(ints(&got["q"]), vec![0, 1, 2, 3], "each step appends the length the queue had: {got}");
}

#[test]
fn edge_a_later_deeper_read_of_the_field_sees_the_value_before_the_write() {
    let step = json!({
        "q": ["array/concat", "@acc.q", ["list", "@i"]],
        "head": ["array/len", "@acc.q"],
        "seen": "@acc.seen"
    });
    let got = eval(queue_fold(5, step));
    assert_eq!(ints(&got["q"]), vec![0, 1, 2, 3, 4]);
    assert_eq!(got["head"].as_f64(), Some(4.0), "the read after the append still sees the old queue: {got}");
}

#[test]
fn edge_a_whole_read_after_the_field_still_sees_the_field() {
    let step = json!({
        "q": ["array/concat", "@acc.q", ["list", "@i"]],
        "head": ["array/len", ["object/get", "@acc", "q"]],
        "seen": "@acc.seen"
    });
    let got = eval(queue_fold(5, step));
    assert_eq!(ints(&got["q"]), vec![0, 1, 2, 3, 4]);
    assert_eq!(got["head"].as_f64(), Some(4.0), "{got}");
}

fn bytes_for(n: i64) -> usize {
    let expr = from_json_value(&queue_fold(n, growing_step())).unwrap();
    let ctx = BindingContextBuilder::new().build();
    let before = alloc_meter::thread_counts();
    Evaluator::new().evaluate(&expr, &ctx).unwrap();
    alloc_meter::thread_counts().since(before).bytes
}

#[test]
fn a_queue_threaded_through_a_fold_grows_in_linear_bytes() {
    bytes_for(10);
    let (small, large) = (bytes_for(500), bytes_for(2000));
    assert!(large < small * 8, "4x the steps cost {large} bytes against {small}: the queue is copied each step");
}

