//! `(behavior/apply value overrides)` on the evaluator — twin of the
//! `@almadar/evaluator` operator.

use orbital_core::evaluator::{eval_expr, BindingContext, Value};
use orbital_core::fixtures::behavior_apply_cases;
use orbital_core::schema::behavior_value::apply_behavior_overrides;
use orbital_core::schema::SExpression;

fn sx(json: &str) -> SExpression {
    serde_json::from_str(json).expect("sexpr json")
}

fn val(json: &str) -> Value {
    serde_json::from_str(json).expect("value json")
}

fn ctx() -> BindingContext {
    let mut c = BindingContext::new();
    c.set_entity(val(
        r#"{"panel":{"behavior":"std/std-kanban","trait":"KanbanBoard"}}"#,
    ));
    c
}

#[test]
fn applies_overrides_to_a_trait_value_binding() {
    let out = eval_expr(
        &sx(r#"["behavior/apply", "@entity.panel", {"config": {"compact": true}}]"#),
        &ctx(),
    )
    .unwrap();
    assert_eq!(
        out,
        val(r#"{"behavior":"std/std-kanban","trait":"KanbanBoard","config":{"compact":true}}"#)
    );
}

#[test]
fn control_a_non_trait_value_is_a_type_error() {
    assert!(eval_expr(&sx(r#"["behavior/apply", "plain", {}]"#), &ctx()).is_err());
    assert!(eval_expr(&sx(r#"["behavior/apply", "@entity.panel", 3]"#), &ctx()).is_err());
}

#[test]
fn control_wrong_arity_is_rejected() {
    assert!(eval_expr(&sx(r#"["behavior/apply", "@entity.panel"]"#), &ctx()).is_err());
}

#[test]
fn unquote_outside_a_quasiquote_is_an_evaluation_error() {
    assert!(eval_expr(&sx(r#"["unquote", "@entity.panel"]"#), &ctx()).is_err());
}

/// Twin of `@almadar/core`'s behavior/apply cases (`fixtures/behavior-apply/cases.json`).
#[test]
fn twin_cases_match_the_core_merge_table() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(behavior_apply_cases()).expect("behavior-apply fixture parses");
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().unwrap_or("?").to_string();
        let value: Value = serde_json::from_value(case["value"].clone()).expect("value");
        let overrides: Value = serde_json::from_value(case["overrides"].clone()).expect("overrides");
        let out = apply_behavior_overrides(&value, &overrides);
        if case["error"] == serde_json::Value::Bool(true) {
            assert!(out.is_none(), "{name}: expected a refusal, got {out:?}");
        } else {
            let expected: Value = serde_json::from_value(case["expected"].clone()).expect("expected");
            assert_eq!(out, Some(expected), "{name}");
        }
    }
}

#[test]
fn applies_overrides_to_an_orbital_value_binding() {
    let mut c = BindingContext::new();
    c.set_entity(val(r#"{"app":{"behavior":"almadar-std/std-kanban","orbital":"KanbanOrbital"}}"#));
    let out = eval_expr(&sx(r#"["behavior/apply", "@entity.app", {"entity": "Task"}]"#), &c).unwrap();
    assert_eq!(out, val(r#"{"behavior":"almadar-std/std-kanban","orbital":"KanbanOrbital","entity":"Task"}"#));
}
