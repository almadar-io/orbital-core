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
    serde_json::from_str(include_str!("../../../../tests/fixtures/operators/json-valid-vectors.json")).unwrap()
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

fn nodes_of(value: serde_json::Value) -> Result<Value, String> {
    let ctx = BindingContextBuilder::new()
        .payload(serde_json::from_value(json!({ "v": value })).unwrap())
        .build();
    let expr: SExpression = serde_json::from_value(json!(["json/nodes", "@payload.v"])).unwrap();
    Evaluator::new().evaluate(&expr, &ctx).map_err(|e| e.to_string())
}

#[test]
fn json_nodes_lists_every_nested_value_in_pre_order() {
    let tree = json!({ "a": [1, { "b": "x" }], "c": null });
    let expected: Value = serde_json::from_value(json!([
        { "a": [1, { "b": "x" }], "c": null },
        [1, { "b": "x" }],
        1,
        { "b": "x" },
        "x",
        null
    ]))
    .unwrap();
    assert_eq!(nodes_of(tree).unwrap(), expected);
}

#[test]
fn json_nodes_keeps_operator_shaped_lists_as_data() {
    let form = json!(["emit", "DONE", { "x": ["call-service", "llm", "embed", {}] }]);
    let nodes = nodes_of(form).unwrap();
    let Value::Array(items) = nodes else { panic!("json/nodes did not return a list") };
    let calls: Vec<&Value> = items
        .iter()
        .filter(|item| matches!(item, Value::Array(parts) if parts.first() == Some(&Value::string("call-service"))))
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(items.len(), 9);
}

#[test]
fn json_nodes_of_a_scalar_is_that_scalar_alone() {
    assert_eq!(nodes_of(json!("leaf")).unwrap(), Value::array(vec![Value::String("leaf".into())]));
    assert_eq!(nodes_of(json!([])).unwrap(), Value::array(vec![Value::Array(vec![].into())]));
}

#[test]
fn json_nodes_takes_exactly_one_argument() {
    let ctx = BindingContextBuilder::new().build();
    let expr: SExpression = serde_json::from_value(json!(["json/nodes", 1, 2])).unwrap();
    assert!(Evaluator::new().evaluate(&expr, &ctx).is_err());
}
