use {orbital_core_eval::evaluator::context::BindingContextBuilder, orbital_core_eval::evaluator::Evaluator, orbital_core_base::value::Value};
use serde::Deserialize;
use serde_json::Value as Json;
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(untagged)]
enum Expected {
    Number(f64),
    Special(String),
}

#[derive(Deserialize)]
struct Case {
    expr: Json,
    expect: Option<Expected>,
    #[serde(default)]
    error: bool,
    source: String,
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join(name)
}

#[test]
fn every_shared_number_vector_matches() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../../../tests/fixtures/operators/number-vectors.json")).unwrap();
    assert!(cases.iter().any(|case| case.error));
    assert!(cases
        .iter()
        .any(|case| matches!(case.expect, Some(Expected::Number(_)))));
    for tag in ["NaN", "Infinity", "-Infinity", "-0"] {
        assert!(cases
            .iter()
            .any(|case| matches!(&case.expect, Some(Expected::Special(value)) if value == tag)));
    }
    let ctx = BindingContextBuilder::new().build();
    for case in cases {
        let expression = serde_json::from_value(case.expr).unwrap();
        let result = Evaluator::new().evaluate(&expression, &ctx);
        if case.error {
            assert!(result.is_err(), "{}: {result:?}", case.source);
            continue;
        }
        let Value::Number(actual) = result.unwrap() else {
            panic!("{}: not a number", case.source)
        };
        match case.expect.unwrap() {
            Expected::Number(expected) => {
                assert_eq!(actual.to_bits(), expected.to_bits(), "{}", case.source)
            }
            Expected::Special(tag) => match tag.as_str() {
                "NaN" => assert!(actual.is_nan(), "{}", case.source),
                "Infinity" => assert_eq!(actual, f64::INFINITY, "{}", case.source),
                "-Infinity" => assert_eq!(actual, f64::NEG_INFINITY, "{}", case.source),
                "-0" => assert_eq!(actual.to_bits(), (-0.0_f64).to_bits(), "{}", case.source),
                _ => panic!("invalid expected number tag: {tag}"),
            },
        }
    }
}

#[test]
fn number_mirror_matches_the_canonical_corpus() {
    let canonical =
        fixture("../../../packages/almadar-parity/fixtures/operators/number-vectors.json");
    if canonical.exists() {
        assert_eq!(
            std::fs::read(canonical).unwrap(),
            std::fs::read(fixture("tests/fixtures/operators/number-vectors.json")).unwrap()
        );
    }
}
