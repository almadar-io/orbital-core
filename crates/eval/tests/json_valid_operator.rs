use {orbital_core_base::schema::SExpression, orbital_core_eval::evaluator::context::BindingContextBuilder, orbital_core_eval::evaluator::Evaluator, orbital_core_base::value::Value};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    expr: SExpression,
    expect: Option<bool>,
    #[serde(default)]
    error: bool,
    source: String,
}

fn cases() -> Vec<Case> {
    serde_json::from_str(include_str!("../../../tests/fixtures/operators/json-valid-vectors.json")).unwrap()
}

#[test]
fn every_shared_json_valid_vector_matches() {
    let cases = cases();
    assert!(cases.iter().any(|case| case.expect == Some(true)));
    assert!(cases.iter().any(|case| case.expect == Some(false)));
    assert!(cases.iter().any(|case| case.error));
    let mut sources = std::collections::HashSet::new();
    let ctx = BindingContextBuilder::new().build();
    for case in cases {
        assert!(
            sources.insert(case.source.clone()),
            "duplicate source: {}",
            case.source
        );
        assert_ne!(
            case.error,
            case.expect.is_some(),
            "invalid fixture: {}",
            case.source
        );
        let result = Evaluator::new().evaluate(&case.expr, &ctx);
        if case.error {
            assert!(result.is_err(), "{}: {result:?}", case.source);
            continue;
        }
        let Value::Bool(actual) = result.unwrap_or_else(|error| panic!("{}: {error}", case.source))
        else {
            panic!("{}: json/valid did not return a boolean", case.source);
        };
        assert_eq!(Some(actual), case.expect, "{}", case.source);
    }
}

#[test]
fn validity_distinguishes_null_without_changing_legacy_parse() {
    let ctx = BindingContextBuilder::new().build();
    for (input, valid) in [("null", true), ("{not json", false)] {
        let parse: SExpression =
            serde_json::from_value(json!(["json/parse", input, "Row"])).unwrap();
        assert_eq!(
            Evaluator::new().evaluate(&parse, &ctx).unwrap(),
            Value::Null
        );
        let check: SExpression = serde_json::from_value(json!(["json/valid", input])).unwrap();
        assert_eq!(
            Evaluator::new().evaluate(&check, &ctx).unwrap(),
            Value::Bool(valid)
        );
    }
}

#[test]
fn legacy_json_parse_controls_remain_unchanged() {
    let ctx = BindingContextBuilder::new().build();
    for (input, expected) in [
        (
            json!("{\"rows\":[1,null,false],\"name\":\"row\"}"),
            json!({"rows": [1, null, false], "name": "row"}),
        ),
        (json!("42"), json!(42)),
        (json!("\"hello\""), json!("hello")),
        (json!("false"), json!(false)),
        (json!("1e400"), json!(null)),
        (json!("null"), json!(null)),
        (json!(""), json!(null)),
        (json!("undefined"), json!(null)),
        (json!(null), json!(null)),
    ] {
        let parse: SExpression =
            serde_json::from_value(json!(["json/parse", input, "Row"])).unwrap();
        let actual = Evaluator::new().evaluate(&parse, &ctx).unwrap();
        let expected: Value = serde_json::from_value(expected).unwrap();
        assert_eq!(actual, expected, "input: {input}");
    }
}
