//! EXEC-EVAL / EXEC-LANG conformance: how the kernel evaluator reads operands, nulls, types and calls.

use orbital_core_base::effect_types::RuntimeEnvironment;
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::value::Value;
use orbital_core_eval::evaluator::{BindingContextBuilder, Evaluator};
use orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache};
use orbital_core_runtime::kernel::RuntimeKernel;
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn value(json: Json) -> Value {
    serde_json::from_value(json).unwrap()
}

fn eval_with(expr: Json, payload: Json) -> Result<Value, String> {
    let ctx = BindingContextBuilder::new().payload(value(payload)).build();
    let expr: SExpression = serde_json::from_value(expr).unwrap();
    Evaluator::new().evaluate(&expr, &ctx).map_err(|e| e.to_string())
}

fn eval(expr: Json) -> Result<Value, String> {
    eval_with(expr, json!({}))
}

fn mistyped() -> Json {
    json!(["array/filter", "not-a-list", ["fn", "x", true]])
}

fn is_type_mismatch(result: &Result<Value, String>) -> bool {
    result.as_ref().is_err_and(|e| e.contains("Type mismatch"))
}

/// EXEC-EVAL-1: the operand count is checked against the declared arity; a missing or extra operand is an error.
#[test]
fn exec_eval_1_an_operand_count_outside_the_declared_arity_is_an_error() {
    let arity = |r: &Result<Value, String>| r.as_ref().is_err_and(|e| e.contains("Arity mismatch"));
    assert!(arity(&eval(json!(["-", 10, 3, 2]))), "an extra operand is refused, never ignored");
    assert!(arity(&eval(json!(["if", true, "a"]))), "a missing operand is refused");
    assert_eq!(eval(json!(["-", 10, 3])), Ok(Value::Number(7.0)), "control: the declared count");
    assert_eq!(eval(json!(["-", 5])), Ok(Value::Number(-5.0)), "control: an omitted optional operand");
}

/// EXEC-EVAL-1: an operator that reads an argument of the wrong kind raises a type mismatch naming both kinds; the
/// kernel runs no separate per-call check of an argument against the declared signature.
#[test]
fn exec_eval_1_an_operator_reading_a_wrong_kind_argument_raises_a_type_mismatch() {
    let mismatch = |expected: &str, actual: &str| Err(format!("Evaluation error: Type mismatch: expected {expected}, got {actual}"));
    assert_eq!(eval(json!(["str/upper", 5])), mismatch("string", "number"));
    assert_eq!(eval(json!(["math/abs", "5"])), mismatch("number", "string"));
    assert_eq!(eval(json!(["<", true, 5])), mismatch("number", "boolean"), "ordering across kinds is an error");
    assert_eq!(eval(json!(["array/flatten", [[1], 5]])), mismatch("array", "number"), "an element the operator reads as a list");
    assert_eq!(eval(json!(["str/upper", "a"])), Ok(Value::from("A")), "control: the kind the operator reads");
    assert_eq!(eval(json!(["math/abs", -5])), Ok(Value::Number(5.0)), "control: the kind the operator reads");
    assert_eq!(eval(json!(["=", 1, "1"])), Ok(Value::Bool(false)), "control: no operator reads a kind here, so nothing is refused (structural equality)");
}

/// EXEC-EVAL-1a: `and` stops at the first false operand; an operand after the stop is never evaluated.
#[test]
fn exec_eval_1a_and_stops_at_the_first_falsy_operand_and_never_checks_the_rest() {
    assert_eq!(eval(json!(["and", true, false, mistyped()])), Ok(Value::Bool(false)), "stopped at false, the mistyped operand never ran");
    assert!(is_type_mismatch(&eval(json!(["and", true, true, mistyped()]))), "control: with no false operand the mistyped one is evaluated");
    assert_eq!(eval(json!(["and", true, true])), Ok(Value::Bool(true)), "all true answers the last");
}

/// EXEC-EVAL-1a: `or` stops at the first true operand; an operand after the stop is never evaluated.
#[test]
fn exec_eval_1a_or_stops_at_the_first_truthy_operand_and_never_checks_the_rest() {
    assert_eq!(eval(json!(["or", false, true, mistyped()])), Ok(Value::Bool(true)), "stopped at true, the mistyped operand never ran");
    assert!(is_type_mismatch(&eval(json!(["or", false, false, mistyped()]))), "control: with no true operand the mistyped one is evaluated");
    assert_eq!(eval(json!(["or", false, false])), Ok(Value::Bool(false)), "all false answers the last");
}

/// EXEC-EVAL-2: null is a type error where the declared type excludes it, and a value where the type admits it.
#[test]
fn exec_eval_2_a_null_argument_is_a_type_error_unless_the_type_admits_null() {
    for expr in [json!(["str/startsWith", null, "a"]), json!(["str/upper", "@payload.missing"]), json!(["math/abs", null])] {
        assert!(is_type_mismatch(&eval(expr.clone())), "null is not a value of the declared type in {expr}");
    }
    assert_eq!(eval(json!(["array/includes", ["a"], null])), Ok(Value::Bool(false)), "control: an item of a type variable may be null");
    assert_eq!(eval(json!(["default", "@payload.missing", "x"])), Ok(Value::from("x")), "control: `default` says what absence means");
    assert!(is_type_mismatch(&eval(json!(["array/includes", "hello", "e"]))), "control: a string where an array belongs is a type error");
}

/// EXEC-EVAL-3a: `array/sort` keys, directions, stability and the non-array error.
#[test]
fn exec_eval_3a_sort_orders_by_key_and_direction() {
    assert_eq!(eval(json!(["array/sort", [3, 1, 2]])), Ok(value(json!([1, 2, 3]))), "dir defaults to asc");
    assert!(eval(json!(["array/sort", [1, 3, 2], null, "bogus"])).is_err(), "a dir other than asc or desc is an error");
    assert_eq!(eval(json!(["array/sort", [1, 3, 2], null, "desc"])), Ok(value(json!([3, 2, 1]))), "control: desc is descending");
    assert_eq!(eval(json!(["array/sort", [1, 3, 2], null, "asc"])), Ok(value(json!([1, 2, 3]))), "control: asc is ascending");
    assert_eq!(eval(json!(["array/sort", [{ "k": 2 }, {}, { "k": 1 }], "k"])), Ok(value(json!([{}, { "k": 1 }, { "k": 2 }]))), "a missing field is null, ordered first");
    assert_eq!(eval(json!(["array/sort", [{ "k": 2 }, { "k": 1 }], ""])), Ok(value(json!([{ "k": 2 }, { "k": 1 }]))), "an empty key sorts the elements, and objects are equal");
    let stable = json!([{ "k": 1, "i": "a" }, { "k": 0, "i": "b" }, { "k": 1, "i": "c" }]);
    assert_eq!(eval(json!(["array/sort", stable, "k", "desc"])), Ok(value(json!([{ "k": 1, "i": "a" }, { "k": 1, "i": "c" }, { "k": 0, "i": "b" }]))), "stable when descending");
    assert!(eval(json!(["array/sort", "abc"])).is_err(), "a non-array first argument is an error");
}

/// EXEC-EVAL-3a: the list holds one element type; unlike element types are a type error, never coerced.
#[test]
fn exec_eval_3a_sort_refuses_unlike_element_types_and_orders_nulls_first() {
    assert!(is_type_mismatch(&eval(json!(["array/sort", [3, "a", true]]))), "unlike element types are a type error");
    assert!(is_type_mismatch(&eval(json!(["array/sort", ["10", 9, "2"]]))), "a number never coerces into a string");
    assert_eq!(eval(json!(["array/sort", ["b", null, "a"]])), Ok(value(json!([null, "a", "b"]))), "control: one type with nulls sorts, nulls first");
}

/// EXEC-EVAL-7: a condition is a boolean; a list or an object there is a type error, never read by truthiness.
#[test]
fn exec_eval_7_a_condition_is_a_boolean_never_truthiness() {
    let pick = |payload: Json| eval_with(json!(["if", "@payload.v", "t", "f"]), json!({ "v": payload }));
    assert!(is_type_mismatch(&pick(json!([]))), "a list condition is a type error");
    assert!(is_type_mismatch(&pick(json!({}))), "an object condition is a type error");
    assert!(is_type_mismatch(&eval(json!(["and", true, 0]))), "a number operand of `and` is a type error");
    assert_eq!(pick(json!(false)), Ok(Value::from("f")), "control: a boolean condition reads as itself");
    assert_eq!(pick(json!(true)), Ok(Value::from("t")), "control: a boolean condition reads as itself");
}

/// EXEC-EVAL-7: a string operator's null operand is a type error, an unset root reads null, NaN and Infinity arrive as null.
#[test]
fn exec_eval_7_string_operators_refuse_null_and_unset_roots_read_null() {
    assert!(is_type_mismatch(&eval(json!(["str/concat", "a", "@payload.missing", "b"]))), "an absent operand is not a string");
    assert_eq!(eval(json!(["str/concat", "a", ["default", "@payload.missing", ""], "b"])), Ok(Value::from("ab")), "control: `default` reads it as empty");
    assert_eq!(eval_with(json!(["str/concat", "a", "@payload.v", "b"]), json!({ "v": "x" })), Ok(Value::from("axb")), "control: a present value is read as itself");
    assert!(is_type_mismatch(&eval(json!(["str/join", ["a", null, "b"], "-"]))), "a null element is not a string");
    assert_eq!(eval(json!(["=", "@payload.missing", null])), Ok(Value::Bool(true)), "an omitted root reads null");

    assert_eq!(serde_json::to_value(Value::Number(f64::NAN)).unwrap(), Json::Null, "NaN arrives as null");
    assert_eq!(serde_json::to_value(Value::Number(f64::INFINITY)).unwrap(), Json::Null, "Infinity arrives as null");
    assert!(serde_json::to_value(Value::Number(1.5)).unwrap().is_number(), "control: a finite number stays a number");
}

fn set_kernel(call: Json) -> RuntimeKernel {
    let kernel = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    kernel
        .register_schema(
            serde_json::from_value(json!({
                "name": "App",
                "orbitals": [{
                    "name": "AppOrbital",
                    "entity": { "name": "Item", "persistence": "runtime",
                        "fields": [{ "name": "id", "type": "string" }, { "name": "list", "type": "array" }] },
                    "traits": [{ "name": "Maker", "linkedEntity": "Item",
                        "stateMachine": { "states": [{ "name": "idle", "isInitial": true }, { "name": "made" }],
                            "transitions": [{ "from": "idle", "to": "made", "event": "MAKE", "effects": [["set", "@entity.list", call]] }] } }]
                }]
            }))
            .unwrap(),
        )
        .unwrap();
    kernel
}

/// EXEC-EVAL-8: an operator-headed array in an effect argument is a call; its failure fails the effect.
#[test]
fn exec_eval_8_a_failing_call_in_an_effect_argument_fails_the_effect_and_the_raw_expression_never_lands() {
    let kernel = set_kernel(mistyped());
    let outcomes = kernel.dispatch("Maker", None, "MAKE", Value::Null).unwrap();
    let (_, result) = outcomes.iter().find(|(t, _)| t == "Maker").expect("the trait ran");
    assert!(result.failure.as_deref().is_some_and(|e| e.contains("Type mismatch")), "the effect failed: {:?}", result.failure);
    let frame = kernel.snapshot_trait("Maker", None).entity;
    assert_ne!(frame.get_path("list"), Some(&value(mistyped())), "the raw expression is not stored");
    assert_eq!(kernel.snapshot_trait("Maker", None).state, "made", "the new state stands (EXEC-STEP-7)");

    let control = set_kernel(json!(["array/filter", ["a", "b"], ["fn", "x", true]]));
    let outcomes = control.dispatch("Maker", None, "MAKE", Value::Null).unwrap();
    let (_, result) = outcomes.iter().find(|(t, _)| t == "Maker").expect("the trait ran");
    assert_eq!(result.failure, None, "control: a well-typed call does not fail the effect");
    assert_eq!(control.snapshot_trait("Maker", None).entity.get_path("list"), Some(&value(json!(["a", "b"]))), "control: the call's result lands");
}

/// EXEC-LANG-2: a type variable is bound by the validator, not per call at run time, so `array/append : [T], T`
/// runs on whatever the validated program hands it.
#[test]
fn exec_lang_2_a_type_variable_is_bound_by_the_validator_and_the_kernel_runs_no_per_call_binding() {
    assert_eq!(eval(json!(["array/append", [1, 2], 3])), Ok(value(json!([1, 2, 3]))), "T = number");
    assert_eq!(eval(json!(["array/append", ["a"], "b"])), Ok(value(json!(["a", "b"]))), "T = string in the next call");
    assert_eq!(eval(json!(["array/append", [1, 2], "x"])), Ok(value(json!([1, 2, "x"]))), "the kernel does not re-check agreement: the validator refuses this pair statically");
    assert!(is_type_mismatch(&eval(json!(["array/append", 5, 1]))), "control: an operator still refuses a non-list where it reads one");
}
