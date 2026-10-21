use {orbital_core_eval::evaluator::context::BindingContextBuilder, orbital_core_eval::evaluator::Evaluator, orbital_core_base::value::Value};
use serde::Deserialize;
use serde_json::Value as Json;

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

const ECMA_NUMBER_TEXT: [(f64, &str); 14] = [
    (1e19, "10000000000000000000"),
    (9.5e18, "9500000000000000000"),
    (1e20, "100000000000000000000"),
    (123456789012345680000.0, "123456789012345680000"),
    (12345678901234567890.0, "12345678901234567000"),
    (1e-6, "0.000001"),
    (1.5e-6, "0.0000015"),
    (1e21, "1e+21"),
    (1e-7, "1e-7"),
    (1e18, "1000000000000000000"),
    (0.0001, "0.0001"),
    (-0.0, "0"),
    (9007199254740993.0, "9007199254740992"),
    (-2.5e-10, "-2.5e-10"),
];

fn eval_text(expr: Json) -> String {
    let ctx = BindingContextBuilder::new().build();
    match Evaluator::new().evaluate(&serde_json::from_value(expr.clone()).unwrap(), &ctx) {
        Ok(Value::String(s)) => s,
        other => panic!("{expr}: {other:?}"),
    }
}

fn ecma_cases() -> Vec<(Json, &'static str)> {
    let mut cases: Vec<(Json, &str)> = ECMA_NUMBER_TEXT.iter().map(|(n, want)| (serde_json::json!(n), *want)).collect();
    cases.push((serde_json::json!(["+", 0.1, 0.2]), "0.30000000000000004"));
    cases
}

#[test]
fn a_number_becomes_text_as_ecmascript_number_to_string() {
    for (number, want) in ecma_cases() {
        assert_eq!(eval_text(serde_json::json!(["str/concat", "", number])), want, "str/concat {number}");
    }
}

#[test]
fn control_str_concat_refuses_an_object_rather_than_stringifying_it() {
    let ctx = BindingContextBuilder::new().build();
    let expr = serde_json::json!(["str/concat", "", {"n": 1e19}]);
    let result = Evaluator::new().evaluate(&serde_json::from_value(expr).unwrap(), &ctx);
    assert!(
        matches!(result, Err(orbital_core_base::error::OrbitalError::Eval(orbital_core_base::error::EvalError::TypeMismatch { .. }))),
        "{result:?}"
    );
}

#[test]
fn json_stringify_writes_a_number_as_ecmascript_number_to_string() {
    for (number, want) in ecma_cases() {
        assert_eq!(eval_text(serde_json::json!(["json/stringify", number])), want, "json/stringify {number}");
        assert_eq!(
            eval_text(serde_json::json!(["json/stringify", {"n": number}])),
            format!("{{\"n\":{want}}}"),
            "json/stringify object {number}"
        );
    }
}

#[test]
fn control_a_non_finite_number_stringifies_as_json_null() {
    let ctx = BindingContextBuilder::new().build();
    for expr in [serde_json::json!(["str/to-number", "Infinity"]), serde_json::json!(["str/to-number", "NaN"])] {
        let stringified = serde_json::json!(["json/stringify", {"n": expr}]);
        let result = Evaluator::new().evaluate(&serde_json::from_value(stringified).unwrap(), &ctx);
        assert_eq!(result.unwrap(), Value::String("{\"n\":null}".to_string()));
    }
}

fn eval_bool(expr: Json) -> bool {
    let ctx = BindingContextBuilder::new().build();
    match Evaluator::new().evaluate(&serde_json::from_value(expr.clone()).unwrap(), &ctx) {
        Ok(Value::Bool(b)) => b,
        other => panic!("{expr}: {other:?}"),
    }
}

#[test]
fn number_equality_is_exact_ieee() {
    assert!(!eval_bool(serde_json::json!(["==", 1e-20, 0])));
    assert!(eval_bool(serde_json::json!(["!=", 1e-20, 0])));
    assert!(!eval_bool(serde_json::json!(["==", ["+", 0.1, 0.2], 0.3])));
    assert!(!eval_bool(serde_json::json!(["==", 1.0000000000000002, 1])));
    assert!(!eval_bool(serde_json::json!(["object/equals", {"n": 1e-20}, {"n": 0}])));
    let nan = serde_json::json!(["str/to-number", "NaN"]);
    assert!(!eval_bool(serde_json::json!(["==", nan, nan])));
    assert!(eval_bool(serde_json::json!(["!=", nan, nan])));
    assert!(!eval_bool(serde_json::json!(["==", nan, 0])));
    assert!(!eval_bool(serde_json::json!(["object/equals", {"n": nan}, {"n": nan}])));
}

#[test]
fn control_equal_numbers_stay_equal_and_ordering_is_unchanged() {
    assert!(eval_bool(serde_json::json!(["==", 0.1, 0.1])));
    assert!(eval_bool(serde_json::json!(["==", 1, 1.0])));
    assert!(eval_bool(serde_json::json!(["==", 0, -0.0])));
    assert!(!eval_bool(serde_json::json!(["!=", 0.1, 0.1])));
    assert!(eval_bool(serde_json::json!(["==", ["*", 3, 0.5], 1.5])));
    assert!(eval_bool(serde_json::json!(["object/equals", {"n": 0.1}, {"n": 0.1}])));
    assert!(eval_bool(serde_json::json!([">", 1e-20, 0])));
    assert!(eval_bool(serde_json::json!(["<=", 0, 1e-20])));
    let inf = serde_json::json!(["str/to-number", "Infinity"]);
    assert!(eval_bool(serde_json::json!(["==", inf, inf])));
    assert!(!eval_bool(serde_json::json!(["!=", inf, inf])));
    assert!(!eval_bool(serde_json::json!(["!=", 0, -0.0])));
    assert!(eval_bool(serde_json::json!(["array/includes", [0.1, 0.5], 0.5])));
}
