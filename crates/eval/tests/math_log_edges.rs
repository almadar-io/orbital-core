// `math/log10` and `math/ln` at 1, 0 and below 0; twin of almadar-evaluator `StdLibraryEvaluator.test.ts`.

use {orbital_core_eval::evaluator::context::BindingContextBuilder, orbital_core_eval::evaluator::Evaluator, orbital_core_base::value::Value};

fn eval(expr: serde_json::Value) -> f64 {
    let ctx = BindingContextBuilder::new().build();
    let sexpr = serde_json::from_value(expr).unwrap();
    match Evaluator::new().evaluate(&sexpr, &ctx).unwrap() {
        Value::Number(n) => n,
        other => panic!("expected a number, got {other:?}"),
    }
}

#[test]
fn both_logs_agree_with_the_js_evaluator_at_one_zero_and_below_zero() {
    for op in ["math/log10", "math/ln"] {
        assert_eq!(eval(serde_json::json!([op, 1])), 0.0, "{op} 1");
        assert_eq!(
            eval(serde_json::json!([op, 0])),
            f64::NEG_INFINITY,
            "{op} 0"
        );
        assert!(eval(serde_json::json!([op, -1])).is_nan(), "{op} -1");
    }
}

#[test]
fn the_bases_differ() {
    assert_eq!(eval(serde_json::json!(["math/log10", 100])), 2.0);
    assert!((eval(serde_json::json!(["math/ln", std::f64::consts::E])) - 1.0).abs() < 1e-12);
}
