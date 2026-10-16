use orbital_core::{
    ir::OirExpr,
    lower::{lower_sexpr, recognized_call_head},
    schema::SExpression,
    BindingContextBuilder, Evaluator, Value,
};
use serde_json::json;

#[test]
fn primitives_and_non_string_heads_are_not_calls() {
    for raw in [json!(null), json!("array/len"), json!([]), json!([0, "@payload"]), json!([{}, 1])] {
        let expr: SExpression = serde_json::from_value(raw).unwrap();
        assert_eq!(recognized_call_head(&expr), None);
    }
}

#[test]
fn unknown_string_heads_keep_all_literal_array_elements() {
    let expr: SExpression = serde_json::from_value(json!(["ordinary literal", "@payload"])).unwrap();
    assert_eq!(recognized_call_head(&expr), None);
    let OirExpr::Array { items } = lower_sexpr(&expr).unwrap() else {
        panic!("unknown head must remain literal data");
    };
    assert_eq!(items.len(), 2);
    assert!(matches!(&items[0], OirExpr::String { value } if value == "ordinary literal"));
    assert!(matches!(&items[1], OirExpr::Binding { binding } if binding.path.is_empty()));
    let payload = Value::Object([("totalCount".into(), Value::Number(7.0))].into_iter().collect());
    let ctx = BindingContextBuilder::new().payload(payload.clone()).build();
    assert_eq!(Evaluator::new().evaluate(&expr, &ctx).unwrap(), Value::Array(vec![Value::String("ordinary literal".into()), payload]));
}

#[test]
fn a_binding_first_element_is_data_not_an_operator_name() {
    let expr: SExpression = serde_json::from_value(json!(["@payload", "ordinary literal"])).unwrap();
    assert_eq!(recognized_call_head(&expr), None);
    let OirExpr::Array { items } = lower_sexpr(&expr).unwrap() else {
        panic!("binding first element must remain literal data");
    };
    assert!(matches!(&items[0], OirExpr::Binding { .. }));
    assert!(matches!(&items[1], OirExpr::String { value } if value == "ordinary literal"));
}

#[test]
fn registered_values_aliases_effects_and_explicit_list_keep_call_identity() {
    for (head, raw) in [
        ("array/len", json!(["array/len", [1, 2]])),
        ("+", json!(["+", 1, 2])),
        ("set", json!(["set", "@entity.n", 1])),
        ("list", json!(["list", "@payload"])),
        ("json/stringify", json!(["json/stringify", "@payload"])),
    ] {
        let expr: SExpression = serde_json::from_value(raw).unwrap();
        assert_eq!(recognized_call_head(&expr), Some(head));
        assert!(lower_sexpr(&expr).is_ok(), "{head}");
    }
}
