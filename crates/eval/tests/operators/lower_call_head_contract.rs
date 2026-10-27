use {orbital_core_base::ir::OirExpr, orbital_core_eval::lower::{lower_sexpr, recognized_call_head}, orbital_core_base::schema::SExpression, orbital_core_eval::evaluator::context::BindingContextBuilder, orbital_core_eval::evaluator::Evaluator, orbital_core_base::value::Value};
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
    assert_eq!(Evaluator::new().evaluate(&expr, &ctx).unwrap(), Value::array(vec![Value::String("ordinary literal".into()), payload]));
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

#[test]
fn an_unknown_namespaced_head_is_refused_by_the_kernel_not_read_as_data() {
    let ctx = BindingContextBuilder::new().build();
    let unknown: SExpression = serde_json::from_value(json!(["example/unknown", 1, 2])).unwrap();
    let error = Evaluator::new().evaluate(&unknown, &ctx).expect_err("an unknown operator is refused");
    assert!(error.to_string().contains("example/unknown"), "{error}");
    let data: SExpression = serde_json::from_value(json!(["ordinary literal", 1])).unwrap();
    assert_eq!(Evaluator::new().evaluate(&data, &ctx).unwrap(), Value::array(vec![Value::String("ordinary literal".into()), Value::Number(1.0)]), "control: a head with no namespace stays data");
    let known: SExpression = serde_json::from_value(json!(["math/abs", -2])).unwrap();
    assert_eq!(Evaluator::new().evaluate(&known, &ctx).unwrap(), Value::Number(2.0), "control: a known namespaced operator runs");
}

#[test]
fn a_slice_with_its_start_past_its_end_answers_empty_text_instead_of_panicking() {
    let ctx = BindingContextBuilder::new().build();
    let backwards: SExpression = serde_json::from_value(json!(["str/slice", "abcdef", 4, 2])).unwrap();
    assert_eq!(Evaluator::new().evaluate(&backwards, &ctx).unwrap(), Value::string(String::new()));
    let forwards: SExpression = serde_json::from_value(json!(["str/slice", "abcdef", 1, 3])).unwrap();
    assert_eq!(Evaluator::new().evaluate(&forwards, &ctx).unwrap(), Value::string("bc"), "control: an ordered range");
}
