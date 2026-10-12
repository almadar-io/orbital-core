//! A whole behavior as a value (`.lolo` type `behavior`) and `(behavior/ref spec)` —
//! twin of `@almadar/core` `behaviorRef` / `isBehaviorRefValue` and the evaluator op.

use orbital_core::evaluator::{eval_expr, BindingContext, Value};
use orbital_core::schema::behavior_value::{behavior_ref, is_behavior_ref_value, is_orbital_value, is_trait_value};
use orbital_core::schema::{FieldType, SExpression};

fn sx(json: &str) -> SExpression {
    serde_json::from_str(json).expect("sexpr json")
}

fn val(json: &str) -> Value {
    serde_json::from_str(json).expect("value json")
}

fn ctx() -> BindingContext {
    let mut c = BindingContext::new();
    c.set_entity(val(r#"{"written":"almadar-behaviors/std-crm","n":3}"#));
    c
}

#[test]
fn behavior_ref_accepts_every_specifier_spelling() {
    for spec in ["almadar-behaviors/std-crm", "std/behaviors/std-kanban", "./orbitals/shop"] {
        assert_eq!(behavior_ref(spec), Some(val(&format!(r#"{{"behavior":"{spec}"}}"#))));
    }
}

#[test]
fn behavior_ref_refuses_a_non_specifier() {
    for bad in ["shop", "../escape/shop", "almadar-behaviors/std-crm.traits.Crm", ""] {
        assert_eq!(behavior_ref(bad), None, "{bad}");
    }
}

#[test]
fn the_guard_is_true_only_for_a_bare_behavior() {
    assert!(is_behavior_ref_value(&val(r#"{"behavior":"./orbitals/shop"}"#)));
    assert!(!is_behavior_ref_value(&val(r#"{"behavior":"./orbitals/shop","trait":"ShopBrowse"}"#)));
    assert!(!is_behavior_ref_value(&val(r#"{"behavior":"./orbitals/shop","orbital":"Shop"}"#)));
    assert!(!is_behavior_ref_value(&val(r#"{"behavior":"not a spec"}"#)));
    assert!(!is_behavior_ref_value(&val(r#"{"type":"stack","behavior":"./orbitals/shop"}"#)));
    assert!(!is_behavior_ref_value(&val(r#""./orbitals/shop""#)));
}

#[test]
fn control_trait_and_orbital_guards_do_not_claim_a_behavior_value() {
    let v = val(r#"{"behavior":"./orbitals/shop"}"#);
    assert!(!is_trait_value(&v));
    assert!(!is_orbital_value(&v));
}

#[test]
fn the_operator_builds_from_a_literal_or_a_binding() {
    assert_eq!(
        eval_expr(&sx(r#"["behavior/ref", "./orbitals/shop"]"#), &ctx()).unwrap(),
        val(r#"{"behavior":"./orbitals/shop"}"#)
    );
    assert_eq!(
        eval_expr(&sx(r#"["behavior/ref", "@entity.written"]"#), &ctx()).unwrap(),
        val(r#"{"behavior":"almadar-behaviors/std-crm"}"#)
    );
}

#[test]
fn the_operator_refuses_a_bad_specifier_and_a_non_string() {
    assert!(eval_expr(&sx(r#"["behavior/ref", "shop"]"#), &ctx()).is_err());
    assert!(eval_expr(&sx(r#"["behavior/ref", "@entity.n"]"#), &ctx()).is_err());
}

#[test]
fn behavior_is_a_field_type() {
    let ft: FieldType = serde_json::from_str(r#""behavior""#).expect("field type");
    assert_eq!(ft, FieldType::Behavior);
}

#[test]
fn behavior_apply_records_a_bridge_on_a_whole_behavior_value() {
    let out = eval_expr(
        &sx(r#"["behavior/apply", {"behavior":"almadar-behaviors/vim-mode"}, {"bridge":{"listens":["SHELL_KEY"],"emits":["STATUS"]}}]"#),
        &ctx(),
    )
    .expect("applies");
    assert_eq!(out, val(r#"{"behavior":"almadar-behaviors/vim-mode","bridge":{"listens":["SHELL_KEY"],"emits":["STATUS"]}}"#));
    assert!(is_behavior_ref_value(&out));
}

#[test]
fn control_a_whole_behavior_value_takes_no_other_override() {
    assert!(eval_expr(&sx(r#"["behavior/apply", {"behavior":"almadar-behaviors/vim-mode"}, {"config":{}}]"#), &ctx()).is_err());
}

#[test]
fn edge_a_malformed_bridge_is_refused_and_not_a_behavior_value() {
    assert!(eval_expr(&sx(r#"["behavior/apply", {"behavior":"almadar-behaviors/vim-mode"}, {"bridge":{"listens":["A"]}}]"#), &ctx()).is_err());
    assert!(!is_behavior_ref_value(&val(r#"{"behavior":"./orbitals/shop","bridge":{"listens":["A"]}}"#)));
    assert!(!is_behavior_ref_value(&val(r#"{"behavior":"./orbitals/shop","bridge":{"listens":[1],"emits":[]}}"#)));
}
