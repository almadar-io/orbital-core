// `(array/sum arr key)` and `(array/avg arr key)` take an optional field name (the std operator table
// declares `maxArity: 2`); twin of almadar-evaluator `evalArraySum` / `evalArrayAvg`.

use {orbital_core_eval::evaluator::context::BindingContextBuilder, orbital_core_eval::evaluator::Evaluator};

fn eval(expr: serde_json::Value) -> serde_json::Value {
    let ctx = BindingContextBuilder::new().build();
    let sexpr = serde_json::from_value(expr).unwrap();
    serde_json::to_value(Evaluator::new().evaluate(&sexpr, &ctx).unwrap()).unwrap()
}

#[test]
fn sum_by_field_adds_that_field_of_each_row() {
    let rows = serde_json::json!([{ "amount": 10 }, { "amount": 20 }, { "amount": 30 }]);
    assert_eq!(
        eval(serde_json::json!(["array/sum", rows, "amount"])),
        serde_json::json!(60.0)
    );
}

#[test]
fn sum_by_field_counts_a_missing_or_null_field_as_nothing() {
    let rows = serde_json::json!([{ "amount": 10 }, { "other": 5 }, { "amount": null }]);
    assert_eq!(
        eval(serde_json::json!(["array/sum", rows, "amount"])),
        serde_json::json!(10.0)
    );
}

#[test]
fn sum_by_field_refuses_a_non_numeric_field() {
    let rows = serde_json::json!([{ "amount": 10 }, { "amount": "x" }]);
    let sexpr = serde_json::from_value(serde_json::json!(["array/sum", rows, "amount"])).unwrap();
    let err = Evaluator::new().evaluate(&sexpr, &BindingContextBuilder::new().build()).unwrap_err();
    assert!(err.to_string().contains("expected number, got string"), "{err}");
}

#[test]
fn control_sum_without_a_field_adds_the_numbers() {
    assert_eq!(
        eval(serde_json::json!(["array/sum", [1, 2, 3]])),
        serde_json::json!(6.0)
    );
}

#[test]
fn avg_by_field_averages_that_field() {
    let rows = serde_json::json!([{ "amount": 10 }, { "amount": 30 }]);
    assert_eq!(
        eval(serde_json::json!(["array/avg", rows, "amount"])),
        serde_json::json!(20.0)
    );
}

#[test]
fn control_avg_without_a_field_averages_the_numbers() {
    assert_eq!(
        eval(serde_json::json!(["array/avg", [2, 4]])),
        serde_json::json!(3.0)
    );
}
