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

mod literal_dotted_keys {
    use super::eval;
    use serde_json::json;

    #[test]
    fn get_reads_an_escaped_dot_as_one_literal_key() {
        let m = json!({ "http.method": "GET", "http": { "method": "nested" } });
        assert_eq!(eval(json!(["object/get", m, "http\\.method"])), json!("GET"));
        assert_eq!(eval(json!(["object/get", m, "http.method"])), json!("nested"), "control: plain a.b is a path");
    }

    #[test]
    fn get_mixes_a_path_and_an_escaped_dot() {
        let m = json!({ "a": { "b.c": 7, "b": { "c": 8 } } });
        assert_eq!(eval(json!(["object/get", m, "a.b\\.c"])), json!(7.0));
        assert_eq!(eval(json!(["object/get", m, "a.b.c"])), json!(8.0));
    }

    #[test]
    fn an_escaped_backslash_is_one_backslash_and_does_not_escape_the_dot() {
        let m = json!({ "a\\": { "b": 1 }, "a\\.b": 2 });
        assert_eq!(eval(json!(["object/get", m, "a\\\\.b"])), json!(1.0));
        assert_eq!(eval(json!(["object/get", m, "a\\\\\\.b"])), json!(2.0));
    }

    #[test]
    fn has_addresses_the_literal_key() {
        let m = json!({ "http.method": "GET" });
        assert_eq!(eval(json!(["object/has", m, "http\\.method"])), json!(true));
        assert_eq!(eval(json!(["object/has", m, "http.method"])), json!(false), "control: plain a.b is a path");
    }

    #[test]
    fn set_writes_the_literal_key_and_a_plain_path_nests() {
        assert_eq!(eval(json!(["object/set", {}, "http\\.method", "POST"])), json!({ "http.method": "POST" }));
        assert_eq!(eval(json!(["object/set", {}, "http.method", "POST"])), json!({ "http": { "method": "POST" } }));
        assert_eq!(eval(json!(["object/set", {}, "a.b\\.c", 1])), json!({ "a": { "b.c": 1.0 } }));
        assert_eq!(eval(json!(["object/set", {}, "a\\\\.b", 1])), json!({ "a\\": { "b": 1.0 } }));
    }
}

#[test]
fn a_binding_path_reads_an_escaped_dot_as_one_literal_key() {
    let ctx = BindingContextBuilder::new()
        .payload(
            serde_json::from_value::<Value>(
                serde_json::json!({ "http.method": "GET", "http": { "method": "nested" }, "a": { "b.c": 7 } }),
            )
            .unwrap(),
        )
        .build();
    let read = |binding: &str| {
        let sexpr = serde_json::from_value(serde_json::json!(binding)).unwrap();
        serde_json::to_value(Evaluator::new().evaluate(&sexpr, &ctx).unwrap()).unwrap()
    };
    assert_eq!(read("@payload.http\\.method"), serde_json::json!("GET"));
    assert_eq!(read("@payload.a.b\\.c"), serde_json::json!(7.0));
    assert_eq!(read("@payload.http.method"), serde_json::json!("nested"), "control: plain a.b is a path");
}
