//! G-CROSS-041: `(quote x)` evaluates to `x` as data on every evaluation path —
//! twin of `@almadar/evaluator` `__tests__/quote.test.ts`.

use {orbital_core_eval::evaluator::{eval_expr, BindingContext}, orbital_core_base::value::Value};
use orbital_core_base::schema::quote::quote_expr;
use orbital_core_base::schema::SExpression;

fn sx(json: &str) -> SExpression {
    serde_json::from_str(json).expect("sexpr json")
}

fn ctx() -> BindingContext {
    let mut c = BindingContext::new();
    c.set_entity(serde_json::from_str::<Value>(r#"{"total":40}"#).unwrap());
    c.set_payload(serde_json::from_str::<Value>(r#"{"amount":7}"#).unwrap());
    c
}

#[test]
fn returns_the_quoted_expression_verbatim_bindings_unresolved() {
    let guard = sx(r#"[">", "@payload.amount", 0]"#);
    assert_eq!(eval_expr(&quote_expr(&guard), &ctx()).unwrap(), Value::from(guard));
    let atom = sx(r#""@entity.total""#);
    assert_eq!(eval_expr(&quote_expr(&atom), &ctx()).unwrap(), Value::String("@entity.total".into()));
}

#[test]
fn control_the_same_expression_unquoted_is_evaluated() {
    assert_eq!(eval_expr(&sx(r#"[">", "@payload.amount", 0]"#), &ctx()).unwrap(), Value::Bool(true));
}

#[test]
fn edge_quoted_effect_with_nested_calls_stays_data() {
    let effect = sx(r#"["set", "@entity.total", ["+", "@entity.total", 1]]"#);
    assert_eq!(eval_expr(&quote_expr(&effect), &ctx()).unwrap(), Value::from(effect));
}

#[test]
fn render_prop_resolution_keeps_the_quoted_data_raw() {
    let guard = sx(r#"[">", "@payload.amount", 0]"#);
    let prop: Value = Value::from(quote_expr(&guard));
    let resolved = ctx().resolve_value(&prop).into_owned();
    assert_eq!(resolved, Value::from(guard));
}
