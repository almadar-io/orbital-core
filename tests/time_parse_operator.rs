//! The Rust half of the shared `time/parse` vectors (`@almadar-io/parity`
//! `fixtures/operators/time-vectors.json`, mirrored under `tests/fixtures/operators/`;
//! the TS twin in `@almadar/evaluator` reads the same file).

use orbital_core::{BindingContextBuilder, Evaluator, Value};
use serde_json::Value as Json;
use std::path::PathBuf;

#[derive(serde::Deserialize)]
struct Case {
    expr: Json,
    #[serde(default)]
    expect: Option<f64>,
    #[serde(default)]
    error: bool,
}

fn crate_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn eval(expr: &Json) -> Result<f64, String> {
    let sexpr = serde_json::from_value(expr.clone()).map_err(|e| e.to_string())?;
    let ctx = BindingContextBuilder::new().build();
    match Evaluator::new().evaluate(&sexpr, &ctx).map_err(|e| e.to_string())? {
        Value::Number(n) => Ok(n),
        other => Err(format!("not a number: {other:?}")),
    }
}

#[test]
fn every_shared_vector_matches() {
    let raw = std::fs::read_to_string(crate_path("tests/fixtures/operators/time-vectors.json")).unwrap();
    let cases: Vec<Case> = serde_json::from_str(&raw).unwrap();
    assert!(cases.iter().any(|c| c.error) && cases.iter().any(|c| !c.error));
    for case in &cases {
        let got = eval(&case.expr);
        if case.error {
            assert!(got.is_err(), "{} must error, got {got:?}", case.expr);
        } else {
            assert_eq!(got.ok(), case.expect, "{}", case.expr);
        }
    }
}

#[test]
fn the_mirror_is_byte_identical_to_the_parity_fixture() {
    let source = crate_path("../../../packages/almadar-parity/fixtures/operators/time-vectors.json");
    if !source.exists() {
        return;
    }
    assert_eq!(
        std::fs::read(source).unwrap(),
        std::fs::read(crate_path("tests/fixtures/operators/time-vectors.json")).unwrap(),
        "re-copy packages/almadar-parity/fixtures/operators/time-vectors.json"
    );
}
