//! `(quasiquote x)` — the codec against the fixture `@almadar/core` owns
//! (`fixtures/quasiquote/cases.json`) and evaluation on the kernel path; twin
//! of `@almadar/core` `__tests__/quasiquote-codec.test.ts`.

use orbital_core::evaluator::ValueMap;
use orbital_core::evaluator::{eval_expr, BindingContext, Value};
use orbital_core::fixtures::quasiquote_cases;
use orbital_core::schema::quote::{
    quasiquote_expr, quasiquote_of, QuasiquoteError, QUASIQUOTE_HEAD,
};
use orbital_core::schema::SExpression;

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    template: SExpression,
    ir: SExpression,
    values: Vec<Value>,
    result: Value,
}

fn cases() -> Vec<Case> {
    serde_json::from_str(quasiquote_cases()).expect("quasiquote fixture parses")
}

fn sx(json: &str) -> SExpression {
    serde_json::from_str(json).expect("sexpr json")
}

/// A context whose bindings hold each case's hole values, so evaluating the IR
/// reproduces the fixture result without the holes being literals.
fn ctx_with(values: &[Value]) -> BindingContext {
    let mut c = BindingContext::new();
    let entity: ValueMap = values
        .iter()
        .enumerate()
        .map(|(i, v)| (format!("h{i}"), v.clone()))
        .collect();
    c.set_entity(Value::Object(entity));
    c
}

#[test]
fn every_case_encodes_to_the_fixture_ir() {
    for c in cases() {
        assert_eq!(
            quasiquote_expr(&c.template).unwrap(),
            c.ir,
            "encode `{}`",
            c.name
        );
    }
}

#[test]
fn every_case_instantiates_to_the_fixture_result() {
    for c in cases() {
        let (body, holes) = quasiquote_of(&c.ir).expect("fixture ir is a quasiquote");
        let mut ir = vec![
            SExpression::String(QUASIQUOTE_HEAD.into()),
            SExpression::String(body.into()),
        ];
        ir.extend((0..holes.len()).map(|i| SExpression::String(format!("@entity.h{i}"))));
        let got = eval_expr(&SExpression::Array(ir), &ctx_with(&c.values)).unwrap();
        assert_eq!(got, c.result, "instantiate `{}`", c.name);
    }
}

#[test]
fn holes_are_evaluated_against_the_live_context() {
    let ir = quasiquote_expr(&sx(
        r#"["set", "@entity.a", ["unquote", ["+", "@entity.h0", 1]]]"#,
    ))
    .unwrap();
    let got = eval_expr(&ir, &ctx_with(&[Value::Number(41.0)])).unwrap();
    assert_eq!(got, Value::from(sx(r#"["set", "@entity.a", 42]"#)));
}

#[test]
fn control_a_template_without_holes_evaluates_to_itself() {
    let tpl = sx(r#"[">", "@payload.amount", 0]"#);
    let got = eval_expr(&quasiquote_expr(&tpl).unwrap(), &BindingContext::new()).unwrap();
    assert_eq!(got, Value::from(tpl));
}

#[test]
fn rejects_nested_quasiquote_and_unquote_inside_a_hole() {
    assert_eq!(
        quasiquote_expr(&sx(r#"["quasiquote", ["a"]]"#)),
        Err(QuasiquoteError::Nested)
    );
    assert_eq!(
        quasiquote_expr(&sx(r#"["unquote", ["unquote", "x"]]"#)),
        Err(QuasiquoteError::UnquoteInUnquote)
    );
    assert_eq!(
        quasiquote_expr(&sx(r#"["unquote"]"#)),
        Err(QuasiquoteError::UnquoteArity)
    );
    assert_eq!(
        quasiquote_expr(&sx(r#"["unquote", "a", "b"]"#)),
        Err(QuasiquoteError::UnquoteArity)
    );
}

#[test]
fn evaluation_rejects_a_hole_count_mismatch() {
    let SExpression::Array(mut ir) = quasiquote_expr(&sx(r#"["unquote", "@entity.h0"]"#)).unwrap()
    else {
        panic!("quasiquote is a call")
    };
    ir.truncate(2);
    assert!(eval_expr(&SExpression::Array(ir), &BindingContext::new()).is_err());
}

#[test]
fn control_quasiquote_of_rejects_quote_and_malformed_calls() {
    assert!(quasiquote_of(&sx(r#"["quote", "\"x\""]"#)).is_none());
    assert!(quasiquote_of(&sx(r#"["quasiquote"]"#)).is_none());
    assert!(quasiquote_of(&sx(r#"["quasiquote", ["not-a-body"]]"#)).is_none());
}

#[test]
fn rejects_a_hole_whose_value_cannot_be_held_as_program_data() {
    let ir = quasiquote_expr(&sx(r#"["x", ["unquote", ["fn", "a", "@a"]]]"#)).unwrap();
    let err = eval_expr(&ir, &BindingContext::new()).unwrap_err();
    assert!(err.to_string().contains("program data"), "{err}");
}
