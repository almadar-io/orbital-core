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

#[test]
fn every_shared_xml_valid_vector_matches() {
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("../../../tests/fixtures/operators/xml-valid-vectors.json")).unwrap();
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
            panic!("{}: xml/valid did not return a boolean", case.source);
        };
        assert_eq!(Some(actual), case.expect, "{}", case.source);
    }
}

#[test]
fn legacy_xml_parse_preserves_empty_text_namespace_and_null_values() {
    let ctx = BindingContextBuilder::new().build();
    for (input, expected) in [
        (json!("<a/>"), json!("")),
        (json!("<a>0</a>"), json!("0")),
        (json!("<a>false</a>"), json!("false")),
        (json!("<a>null</a>"), json!("null")),
        (
            json!("<x:r xmlns:x=\"urn:x\"><x:a>v</x:a></x:r>"),
            json!({"a": "v"}),
        ),
        (json!(null), json!(null)),
    ] {
        let parse: SExpression =
            serde_json::from_value(json!(["xml/parse", input, "Row"])).unwrap();
        let expected: Value = serde_json::from_value(expected).unwrap();
        assert_eq!(Evaluator::new().evaluate(&parse, &ctx).unwrap(), expected);
    }
}

#[test]
fn legacy_xml_parse_still_throws_for_malformed_syntax() {
    let ctx = BindingContextBuilder::new().build();
    for input in ["", "plain text", "<a>", "<a></b>", "<a/><b/>"] {
        let parse: SExpression =
            serde_json::from_value(json!(["xml/parse", input, "Row"])).unwrap();
        assert!(Evaluator::new().evaluate(&parse, &ctx).is_err(), "{input}");
    }
}
