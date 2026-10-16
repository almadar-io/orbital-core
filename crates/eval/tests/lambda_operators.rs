// Every lambda-taking operator applies its `(fn …)` per element, never treating the closure as a truthy value. Twin of almadar-evaluator `lambda-operators.test.ts` (identical table).

use {orbital_core_eval::evaluator::context::BindingContextBuilder, orbital_core_eval::evaluator::Evaluator};
use serde_json::{json, Value as Json};

fn gt(n: i64) -> Json {
    json!(["fn", "x", [">", "@x", n]])
}

/// Integral f64s as integers, so `[2.0]` compares equal to `[2]` like the JS twin.
fn normalize(v: Json) -> Json {
    match v {
        Json::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 => json!(f as i64),
            _ => Json::Number(n),
        },
        Json::Array(a) => Json::Array(a.into_iter().map(normalize).collect()),
        Json::Object(m) => Json::Object(m.into_iter().map(|(k, v)| (k, normalize(v))).collect()),
        other => other,
    }
}

fn eval(expr: Json) -> Json {
    let ctx = BindingContextBuilder::new().build();
    let sexpr = serde_json::from_value(expr).unwrap();
    normalize(serde_json::to_value(Evaluator::new().evaluate(&sexpr, &ctx).unwrap()).unwrap())
}

#[test]
fn every_lambda_operator_applies_its_fn() {
    let cases: Vec<(&str, Json, Json)> = vec![
        (
            "array/count",
            json!(["array/count", ["list", 1, 2, 3], gt(1)]),
            json!(2),
        ),
        (
            "array/every true",
            json!(["array/every", ["list", 2, 3], gt(1)]),
            json!(true),
        ),
        (
            "array/every false",
            json!(["array/every", ["list", 1, 2], gt(5)]),
            json!(false),
        ),
        (
            "array/filter",
            json!(["array/filter", ["list", 1, 2, 3], gt(1)]),
            json!([2, 3]),
        ),
        (
            "array/find",
            json!(["array/find", ["list", 1, 2, 3], gt(1)]),
            json!(2),
        ),
        (
            "array/findIndex",
            json!(["array/findIndex", ["list", 1, 2, 3], gt(1)]),
            json!(1),
        ),
        (
            "array/map",
            json!(["array/map", ["list", 1, 2], ["fn", "x", ["*", "@x", 2]]]),
            json!([2, 4]),
        ),
        (
            "array/partition",
            json!(["array/partition", ["list", 1, 2, 3], gt(1)]),
            json!([[2, 3], [1]]),
        ),
        (
            "array/reduce",
            json!([
                "array/reduce",
                ["list", 1, 2, 3],
                0,
                ["fn", ["acc", "x"], ["+", "@acc", "@x"]]
            ]),
            json!(6),
        ),
        (
            "array/reject",
            json!(["array/reject", ["list", 1, 2, 3], gt(1)]),
            json!([1]),
        ),
        (
            "array/some true",
            json!(["array/some", ["list", 1, 9], gt(5)]),
            json!(true),
        ),
        (
            "array/some false",
            json!(["array/some", ["list", 1, 2], gt(5)]),
            json!(false),
        ),
        (
            "object/filter",
            json!(["object/filter", { "a": 1, "b": 2 }, ["fn", ["k", "v"], [">", "@v", 1]]]),
            json!({ "b": 2 }),
        ),
        (
            "object/mapKeys",
            json!(["object/mapKeys", { "a": 1 }, ["fn", "k", ["str/upper", "@k"]]]),
            json!({ "A": 1 }),
        ),
        (
            "object/mapValues",
            json!(["object/mapValues", { "a": 1, "b": 2 }, ["fn", "v", ["*", "@v", 10]]]),
            json!({ "a": 10, "b": 20 }),
        ),
    ];
    let failures: Vec<String> = cases
        .into_iter()
        .filter_map(|(name, expr, want)| {
            let got = eval(expr);
            (got != want).then(|| format!("{name}: got {got}, want {want}"))
        })
        .collect();
    assert!(failures.is_empty(), "{failures:#?}");
}
