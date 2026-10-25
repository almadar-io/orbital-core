//! The Rust half of the shared `time/parse` vectors (orbital-core
//! `fixtures/operators/time-vectors.json`, mirrored under `tests/fixtures/operators/`;
//! the TS twin in `@almadar/evaluator` reads the same file).

use {orbital_core_eval::evaluator::context::BindingContextBuilder, orbital_core_eval::evaluator::Evaluator, orbital_core_base::value::Value};
use serde_json::Value as Json;
use std::path::PathBuf;

#[derive(serde::Deserialize)]
struct Case {
    expr: Json,
    #[serde(default)]
    expect: Option<Json>,
    #[serde(default)]
    error: bool,
    #[serde(default)]
    null: bool,
}

fn crate_path(rel: &str) -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join(rel)
}

fn eval(expr: &Json) -> Result<Json, String> {
    let sexpr = serde_json::from_value(expr.clone()).map_err(|e| e.to_string())?;
    let ctx = BindingContextBuilder::new().build();
    match Evaluator::new().evaluate(&sexpr, &ctx).map_err(|e| e.to_string())? {
        Value::Number(n) => Ok(Json::from(n as i64)),
        Value::Bool(b) => Ok(Json::Bool(b)),
        Value::Null => Ok(Json::Null),
        other => Err(format!("not a number or boolean: {other:?}")),
    }
}

#[test]
fn every_shared_vector_matches() {
    let raw = std::fs::read_to_string(crate_path("tests/fixtures/operators/time-vectors.json")).unwrap();
    let cases: Vec<Case> = serde_json::from_str(&raw).unwrap();
    assert!(cases.iter().any(|c| c.null) && cases.iter().any(|c| !c.null && !c.error));
    for case in &cases {
        let got = eval(&case.expr);
        if case.null {
            assert_eq!(got, Ok(Json::Null), "{} must answer null", case.expr);
        } else if case.error {
            assert!(got.is_err(), "{} must error, got {got:?}", case.expr);
        } else {
            assert_eq!(got.ok(), case.expect, "{}", case.expr);
        }
    }
}
