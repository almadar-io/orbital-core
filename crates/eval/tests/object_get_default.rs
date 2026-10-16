// `(object/get obj path default)` returns the evaluated default when the path is absent; twin of almadar-evaluator `StdLibraryEvaluator.test.ts` "object/get default".

use {orbital_core_eval::evaluator::context::BindingContextBuilder, orbital_core_eval::evaluator::Evaluator, orbital_core_base::value::Value};

fn eval(expr: serde_json::Value) -> serde_json::Value {
    let ctx = BindingContextBuilder::new()
        .payload(
            serde_json::from_value::<Value>(serde_json::json!({ "fallback": "from-payload" }))
                .unwrap(),
        )
        .build();
    let sexpr = serde_json::from_value(expr).unwrap();
    serde_json::to_value(Evaluator::new().evaluate(&sexpr, &ctx).unwrap()).unwrap()
}

#[test]
fn a_missing_key_takes_the_default() {
    assert_eq!(
        eval(serde_json::json!(["object/get", { "a": 1 }, "b", "dflt"])),
        serde_json::json!("dflt")
    );
}

#[test]
fn the_default_is_an_evaluated_expression() {
    assert_eq!(
        eval(serde_json::json!([
            "object/get",
            {},
            "kind",
            "@payload.fallback"
        ])),
        serde_json::json!("from-payload")
    );
}

#[test]
fn a_missing_intermediate_takes_the_default() {
    assert_eq!(
        eval(serde_json::json!(["object/get", { "a": {} }, "a.b.c", "none"])),
        serde_json::json!("none")
    );
}

#[test]
fn a_present_value_wins_over_the_default_even_when_falsy() {
    assert_eq!(
        eval(serde_json::json!(["object/get", { "k": "" }, "k", "dflt"])),
        serde_json::json!("")
    );
    assert_eq!(
        eval(serde_json::json!(["object/get", { "k": false }, "k", true])),
        serde_json::json!(false)
    );
}

#[test]
fn without_a_default_a_missing_key_is_null() {
    assert_eq!(
        eval(serde_json::json!(["object/get", { "a": 1 }, "b"])),
        serde_json::Value::Null
    );
}
