#[cfg(not(feature = "std"))]
use std::prelude::v1::*;
#[allow(unused_imports)]
use orbital_core_base::ir::expr::*;

#[test]
fn to_sexpression_guard_roundtrips_through_evaluator() {
    use orbital_core_base::schema::sexpr::SExpression as S;
    use {
        crate::evaluator::context::BindingContext, crate::evaluator::Evaluator,
        orbital_core_base::value::Value,
    };

    // `(>= (/ @payload.current @entity.max) 0.85)` — a ratio guard.
    let guard = OirExpr::BinaryOp {
        op: OirBinaryOp::Ge,
        left: Box::new(OirExpr::BinaryOp {
            op: OirBinaryOp::Div,
            left: Box::new(OirExpr::payload_binding(vec!["current".into()])),
            right: Box::new(OirExpr::entity_binding(vec!["max".into()])),
        }),
        right: Box::new(OirExpr::Number { value: 0.85 }),
    };
    // Structurally bridges to the canonical operator spellings.
    let sx = guard.to_sexpression().unwrap();
    assert_eq!(
        sx,
        S::Array(vec![
            S::String(">=".into()),
            S::Array(vec![
                S::String("/".into()),
                S::String("@payload.current".into()),
                S::String("@entity.max".into()),
            ].into()),
            S::Number(0.85),
        ].into())
    );
    // And the one evaluator agrees: 90/100 = 0.9 ≥ 0.85.
    let v = |j: serde_json::Value| serde_json::from_value::<Value>(j).unwrap();
    let mut ctx = BindingContext::new();
    ctx.set_payload(v(serde_json::json!({ "current": 90 })));
    ctx.set_entity(v(serde_json::json!({ "max": 100 })));
    assert!(Evaluator::new().evaluate_guard_oir(&guard, &ctx).unwrap());
    ctx.set_payload(v(serde_json::json!({ "current": 50 })));
    assert!(!Evaluator::new().evaluate_guard_oir(&guard, &ctx).unwrap());
}
