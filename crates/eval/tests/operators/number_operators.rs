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
    #[serde(default)]
    null: bool,
    source: String,
}

#[test]
fn every_shared_number_vector_matches() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../../../../tests/fixtures/operators/number-vectors.json")).unwrap();
    assert!(cases.iter().any(|case| case.error));
    assert!(cases
        .iter()
        .any(|case| matches!(case.expect, Some(Expected::Number(_)))));
    assert!(cases.iter().any(|case| case.null));
    for tag in ["Infinity", "-Infinity", "-0"] {
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
        if case.null {
            assert_eq!(result.unwrap(), Value::Null, "{}", case.source);
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
        Ok(Value::String(s)) => s.into_inner(),
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
    let inf = serde_json::json!(["str/to-number", "Infinity"]);
    for expr in [inf.clone(), serde_json::json!(["-", inf, inf])] {
        let stringified = serde_json::json!(["json/stringify", {"n": expr}]);
        let result = Evaluator::new().evaluate(&serde_json::from_value(stringified).unwrap(), &ctx);
        assert_eq!(result.unwrap(), Value::string("{\"n\":null}"));
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
    let inf = serde_json::json!(["str/to-number", "Infinity"]);
    let nan = serde_json::json!(["-", inf, inf]);
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

fn eval_any(expr: Json) -> Value {
    let ctx = BindingContextBuilder::new().build();
    Evaluator::new().evaluate(&serde_json::from_value(expr).unwrap(), &ctx).unwrap()
}

#[test]
fn nan_results_answer_null_and_infinities_stay() {
    for expr in [
        serde_json::json!(["math/sqrt", -1]),
        serde_json::json!(["math/ln", -1]),
        serde_json::json!(["math/log10", -1]),
        serde_json::json!(["math/asin", 2]),
        serde_json::json!(["math/acos", 2]),
        serde_json::json!(["math/pow", -8, 0.5]),
        serde_json::json!(["str/to-number", "n/a"]),
    ] {
        assert_eq!(eval_any(expr.clone()), Value::Null, "{expr}");
    }
    assert_eq!(eval_any(serde_json::json!(["math/sqrt", 16])), Value::Number(4.0));
    assert_eq!(eval_any(serde_json::json!(["math/pow", 2, 8])), Value::Number(256.0));
    assert_eq!(eval_any(serde_json::json!(["math/asin", 1])), Value::Number(std::f64::consts::FRAC_PI_2));
    assert_eq!(eval_any(serde_json::json!(["math/ln", 0])), Value::Number(f64::NEG_INFINITY));
    assert_eq!(eval_any(serde_json::json!(["math/pow", 10, 400])), Value::Number(f64::INFINITY));
    assert_eq!(eval_any(serde_json::json!(["str/to-number", "5"])), Value::Number(5.0));
}

#[test]
fn division_and_range_failures_answer_null_with_a_good_control() {
    for expr in [
        serde_json::json!(["/", 1, 0]),
        serde_json::json!(["%", 1, 0]),
        serde_json::json!(["math/mod", 1, 0]),
        serde_json::json!(["math/randomInt", 5, 1]),
        serde_json::json!(["bit/xor", 4294967296_i64, 0]),
        serde_json::json!(["bit/shl", 1, 32]),
        serde_json::json!(["bytes/at", "aGk=", 1.5]),
        serde_json::json!(["bytes/read-uint", "EjQ=", 0, 3, "be"]),
        serde_json::json!(["bytes/write-uint", 256, 1, "be"]),
        serde_json::json!(["hash/pbkdf2", "cGFzc3dvcmQ=", "c2FsdA==", 0, 32]),
        serde_json::json!(["format/number", 1, {"decimals": 101}]),
        serde_json::json!(["format/percent", 0.5, 101]),
        serde_json::json!(["format/currency", 1, "XYZ"]),
        serde_json::json!(["format/bytes", -1]),
        serde_json::json!(["prob/histogram", [1, 2], 0]),
        serde_json::json!(["prob/percentile", [1, 2], 101]),
        serde_json::json!(["prob/credible-interval", [1, 2], 2]),
    ] {
        assert_eq!(eval_any(expr.clone()), Value::Null, "{expr}");
    }
    assert_eq!(eval_any(serde_json::json!(["/", 6, 3])), Value::Number(2.0));
    assert_eq!(eval_any(serde_json::json!(["%", 7, 3])), Value::Number(1.0));
    assert_eq!(eval_any(serde_json::json!(["math/randomInt", 5, 5])), Value::Number(5.0));
    assert_eq!(eval_any(serde_json::json!(["bit/xor", 1, 3])), Value::Number(2.0));
    assert_eq!(eval_any(serde_json::json!(["bit/shl", 1, 3])), Value::Number(8.0));
    assert_eq!(eval_any(serde_json::json!(["bytes/at", "aGk=", 1])), Value::Number(105.0));
    assert_eq!(eval_any(serde_json::json!(["bytes/read-uint", "EjQ=", 0, 2, "be"])), Value::Number(4660.0));
    assert_eq!(eval_any(serde_json::json!(["bytes/write-uint", 255, 1, "be"])), Value::string("/w=="));
    assert!(matches!(eval_any(serde_json::json!(["hash/pbkdf2", "cGFzc3dvcmQ=", "c2FsdA==", 1, 32])), Value::String(_)));
    assert_eq!(eval_any(serde_json::json!(["format/number", 1234, {"decimals": 1}])), Value::string("1,234.0"));
    assert_eq!(eval_any(serde_json::json!(["format/percent", 0.5, 1])), Value::string("50.0%"));
    assert_eq!(eval_any(serde_json::json!(["format/currency", 1, "USD"])), Value::string("$1.00"));
    assert_eq!(eval_any(serde_json::json!(["format/bytes", 2048])), Value::string("2 KB"));
    assert_eq!(eval_any(serde_json::json!(["prob/percentile", [1, 3], 50])), Value::Number(2.0));
    assert!(matches!(eval_any(serde_json::json!(["prob/histogram", [1, 2], 2])), Value::Object(_)));
    assert!(matches!(eval_any(serde_json::json!(["prob/credible-interval", [1, 2, 3], 0.5])), Value::Array(_)));
}
