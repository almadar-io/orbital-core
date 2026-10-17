// D-12: a present argument that contradicts the type the operator table declares for its position raises the
// typed `TypeMismatch`. One fixture, read by this suite and by `@almadar/evaluator` `operator-types.test.ts`
// (canonical source `@almadar-io/parity` `fixtures/operator-types`, mirrored into `tests/baked/`).

use {orbital_core_eval::evaluator::context::BindingContextBuilder, orbital_core_base::error::{EvalError, OrbitalError}, orbital_core_eval::evaluator::Evaluator, orbital_core_base::value::Value};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize)]
struct Mismatch {
    kind: String,
    expected: String,
    actual: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum Expect {
    Value(serde_json::Value),
    Error(Mismatch),
}

#[derive(Deserialize)]
struct Case {
    name: String,
    expr: serde_json::Value,
    entity: Option<serde_json::Value>,
    expect: Expect,
}

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

fn fixture() -> Fixture {
    let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .join("tests/baked/operator-types/operator-types.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} unreadable: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("operator-types.json malformed: {e}"))
}

fn run(case: &Case) -> Result<serde_json::Value, OrbitalError> {
    let mut builder = BindingContextBuilder::new();
    if let Some(entity) = &case.entity {
        builder = builder.entity(serde_json::from_value::<Value>(entity.clone()).unwrap());
    }
    let sexpr = serde_json::from_value(case.expr.clone()).unwrap();
    let value = Evaluator::new().evaluate(&sexpr, &builder.build())?;
    Ok(serde_json::to_value(value).unwrap())
}

/// JSON numbers compare by value within a float epsilon, so `3` and `3.0` agree and a trig result may differ in the last digit.
fn same(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    match (a, b) {
        (serde_json::Value::Number(x), serde_json::Value::Number(y)) => {
            let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            (x - y).abs() <= 1e-12 * 1f64.max(x.abs()).max(y.abs())
        }
        (serde_json::Value::Array(x), serde_json::Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (serde_json::Value::Object(x), serde_json::Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, p)| y.get(k).is_some_and(|q| same(p, q)))
        }
        _ => a == b,
    }
}

#[test]
fn every_shared_case_agrees() {
    let fixture = fixture();
    assert!(
        fixture.cases.len() > 100,
        "the fixture is the contract; it must not shrink to nothing"
    );
    let mut failures = Vec::new();
    for case in &fixture.cases {
        match (&case.expect, run(case)) {
            (Expect::Value(want), Ok(got)) => {
                if !same(want, &got) {
                    failures.push(format!("{}: want {want}, got {got}", case.name));
                }
            }
            (Expect::Value(want), Err(e)) => {
                failures.push(format!("{}: want {want}, got error {e}", case.name))
            }
            (Expect::Error(m), Ok(got)) => {
                failures.push(format!(
                    "{}: want TypeMismatch({} got {}), got value {got}",
                    case.name, m.expected, m.actual
                ));
            }
            (
                Expect::Error(m),
                Err(OrbitalError::Eval(EvalError::TypeMismatch { expected, actual })),
            ) => {
                assert_eq!(m.kind, "TypeMismatch");
                if *expected != m.expected || *actual != m.actual {
                    failures.push(format!(
                        "{}: want expected={} actual={}, got expected={expected} actual={actual}",
                        case.name, m.expected, m.actual
                    ));
                }
            }
            (Expect::Error(m), Err(e)) => failures.push(format!(
                "{}: want TypeMismatch({}), got {e}",
                case.name, m.expected
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n{}",
        failures.len(),
        fixture.cases.len(),
        failures.join("\n")
    );
}

#[test]
fn the_message_shape_is_the_twin_of_the_js_error() {
    let sexpr = serde_json::from_value(serde_json::json!(["str/upper", 5])).unwrap();
    let err = Evaluator::new()
        .evaluate(&sexpr, &BindingContextBuilder::new().build())
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Evaluation error: Type mismatch: expected string, got number"
    );
}

fn on_mixed_map(expr: serde_json::Value) -> Result<serde_json::Value, OrbitalError> {
    let case = Case {
        name: String::new(),
        expr,
        entity: Some(serde_json::json!({ "a": 1, "b": "x", "c": 2 })),
        expect: Expect::Value(serde_json::Value::Null),
    };
    let _ = &case.name;
    run(&case)
}

#[test]
fn a_list_of_unlike_entry_tuples_is_a_list_for_structural_array_operators() {
    let entries = serde_json::json!(["object/entries", "@entity"]);
    assert_eq!(on_mixed_map(serde_json::json!(["array/count", entries])).unwrap(), serde_json::json!(3.0));
    assert_eq!(on_mixed_map(serde_json::json!(["array/nth", entries, 1])).unwrap(), serde_json::json!(["b", "x"]));
    assert_eq!(
        on_mixed_map(serde_json::json!(["array/count", ["array/concat", entries, entries]])).unwrap(),
        serde_json::json!(6.0)
    );
}

#[test]
fn control_a_list_of_unlike_scalars_is_still_refused_by_an_ordering_operator() {
    let mixed = on_mixed_map(serde_json::json!(["array/max", ["array/nth", ["object/entries", "@entity"], 0]]));
    assert!(matches!(mixed, Err(OrbitalError::Eval(EvalError::TypeMismatch { .. }))), "{mixed:?}");
}
