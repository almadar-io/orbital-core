use {orbital_core_base::error::{EvalError, OrbitalError}, orbital_core_base::schema::{SExpression, SequentialBindings}, orbital_core_eval::evaluator::context::{BindingContext, BindingContextBuilder}, orbital_core_eval::evaluator::Evaluator, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};

fn args(value: Json) -> Vec<SExpression> {
    serde_json::from_value(value).unwrap()
}

fn evaluate(arguments: Vec<SExpression>, ctx: &BindingContext) -> Result<Value, OrbitalError> {
    let mut call = vec![SExpression::string("let")];
    call.extend(arguments);
    Evaluator::new().evaluate(&SExpression::Array(call.into()), ctx)
}

fn ignored_slots() -> Vec<SExpression> {
    args(json!([
        [null, false, 7, "text", {}, [], ["onlyName"],
            [9, ["base64/decode", "!"]],
            ["kept", 1, ["base64/decode", "!"]]],
        "@kept",
        ["base64/decode", "!"]
    ]))
}

#[test]
fn sequential_binding_view_preserves_order_duplicates_and_arbitrary_string_names() {
    let arguments = args(json!([
        [["x", 1], ["x", ["+", "@x", 1]], ["", 2], ["base64/encode", 3],
            ["اسم", 4], ["a.b", 5], ["@name", 6]],
        "@x"
    ]));
    let view = SequentialBindings::from_args(&arguments).unwrap();
    let names: Vec<&str> = view.bindings.iter().map(|(name, _, _)| name.as_str()).collect();
    assert_eq!(names, ["x", "x", "", "base64/encode", "اسم", "a.b", "@name"]);
    let pairs = arguments[0].as_array().unwrap();
    for (index, (_, value, _)) in view.bindings.iter().enumerate() {
        assert!(std::ptr::eq(*value, &pairs[index].as_array().unwrap()[1]));
    }
    assert!(std::ptr::eq(view.body, &arguments[1]));
}

#[test]
fn sequential_binding_view_refuses_a_malformed_entry() {
    let arguments = ignored_slots();
    assert!(SequentialBindings::from_args(&arguments).is_none(), "an entry that is not [name value] makes the bindings malformed");
    let arguments = args(json!([[["kept", 1]], "@kept"]));
    let view = SequentialBindings::from_args(&arguments).unwrap();
    assert_eq!(view.bindings.len(), 1, "control: well-formed pairs");
    assert_eq!(view.bindings[0].0, "kept");
    assert_eq!(view.bindings[0].1, &SExpression::Number(1.0));
    assert_eq!(view.body, &SExpression::string("@kept"));
    assert!(std::ptr::eq(view.body, &arguments[1]));
}

#[test]
fn sequential_binding_view_reads_an_optional_declared_shape_and_refuses_other_third_items() {
    let arguments = args(json!([[["x", 1, "{\"type\":\"number\"}"], ["y", 2]], "@x"]));
    let view = SequentialBindings::from_args(&arguments).unwrap();
    assert_eq!(view.bindings[0].2, Some("{\"type\":\"number\"}"));
    assert_eq!(view.bindings[1].2, None, "control: a pair declares no shape");
    for malformed in [json!([[["x", 1, 2]], "@x"]), json!([[["x", 1, "s", "t"]], "@x"]), json!([[["x", 1, {}]], "@x"])] {
        assert!(SequentialBindings::from_args(&args(malformed)).is_none());
    }
}

#[test]
fn a_let_with_a_declared_shape_evaluates_exactly_like_the_pair() {
    let ctx = BindingContextBuilder::new().build();
    let declared = evaluate(args(json!([[["x", 2, "{\"type\":\"number\"}"]], ["+", "@x", 1]])), &ctx).unwrap();
    let pair = evaluate(args(json!([[["x", 2]], ["+", "@x", 1]])), &ctx).unwrap();
    assert_eq!(declared, Value::Number(3.0));
    assert_eq!(declared, pair);
}

#[test]
fn sequential_binding_view_requires_a_bindings_array_and_a_body() {
    for invalid in [
        json!([]), json!([[]]), json!([null, 1]), json!([true, 1]),
        json!([7, 1]), json!(["bindings", 1]), json!([{}, 1]),
    ] {
        assert!(SequentialBindings::from_args(&args(invalid)).is_none());
    }
    let arguments = args(json!([[], null]));
    let view = SequentialBindings::from_args(&arguments).unwrap();
    assert!(view.bindings.is_empty());
    assert_eq!(view.body, &SExpression::Null);
}

#[test]
fn sequential_bindings_wrapper_recognizes_only_the_canonical_let_form() {
    for invalid in [
        json!(null), json!("let"), json!([]), json!([1, [], null]),
        json!(["Let", [], null]), json!(["sequential", [], null]),
        json!(["let"]), json!(["let", []]), json!(["let", {}, null]),
    ] {
        let expression: SExpression = serde_json::from_value(invalid).unwrap();
        assert!(expression.sequential_bindings().is_none());
    }
    let expression: SExpression = serde_json::from_value(json!([
        "let", [["x", 1]], null, ["base64/decode", "!"]
    ])).unwrap();
    let view = expression.sequential_bindings().unwrap();
    assert_eq!(view.bindings.iter().map(|(name, value, _)| (name.as_str(), *value)).collect::<Vec<_>>(), [("x", &SExpression::Number(1.0))]);
    assert_eq!(view.body, &SExpression::Null);
}

#[test]
fn real_let_initializers_see_earlier_values_and_shadow_after_evaluation() {
    let ctx = BindingContextBuilder::new()
        .local("x", Value::Number(100.0))
        .local("outer", Value::Number(9.0))
        .build();
    let result = evaluate(args(json!([
        [["x", ["+", "@x", 1]], ["y", ["+", "@x", 2]], ["x", ["+", "@y", 3]]],
        ["list", "@x", "@y", "@outer"]
    ])), &ctx).unwrap();
    assert_eq!(result, Value::array(vec![Value::Number(106.0), Value::Number(103.0), Value::Number(9.0)]));
    assert_eq!(ctx.resolve_binding("@x"), Some(Value::Number(100.0)));
    assert_eq!(ctx.resolve_binding("@outer"), Some(Value::Number(9.0)));
    assert_eq!(ctx.resolve_binding("@y"), None);
}

#[test]
fn real_let_refuses_an_extra_slot_and_malformed_entries() {
    let ctx = BindingContextBuilder::new().build();
    assert!(matches!(
        evaluate(ignored_slots(), &ctx),
        Err(OrbitalError::Eval(EvalError::ArityMismatch { ref operator, actual: 3, .. })) if operator == "let"
    ));
    let mut no_extra = ignored_slots();
    no_extra.pop();
    assert!(
        matches!(evaluate(no_extra, &ctx), Err(OrbitalError::Eval(EvalError::InvalidArgument(_)))),
        "malformed entries make the bindings malformed, never skipped"
    );
    assert_eq!(evaluate(args(json!([[["kept", 1]], "@kept"])), &ctx).unwrap(), Value::Number(1.0), "control");
    assert_eq!(ctx.resolve_binding("@kept"), None);
}

#[test]
fn real_let_does_evaluate_valid_initializer_and_body_slots() {
    let ctx = BindingContextBuilder::new().build();
    for arguments in [
        args(json!([[["x", ["base64/decode"]]], "@x"])),
        args(json!([[], ["base64/decode"]])),
    ] {
        match evaluate(arguments, &ctx) {
            Err(OrbitalError::Eval(EvalError::ArityMismatch { operator, .. })) => {
                assert_eq!(operator, "base64/decode");
            }
            other => panic!("expected the selected expression to fail, got {other:?}"),
        }
    }
}

#[test]
fn real_let_retains_null_body_and_arity_before_bindings_shape_errors() {
    let ctx = BindingContextBuilder::new().build();
    assert_eq!(evaluate(args(json!([[], null])), &ctx).unwrap(), Value::Null);
    for arguments in [args(json!([])), args(json!([[]])), args(json!([7]))] {
        let count = arguments.len();
        match evaluate(arguments, &ctx) {
            Err(OrbitalError::Eval(EvalError::ArityMismatch { operator, expected, actual })) => {
                assert_eq!(operator, "let");
                assert_eq!(expected, 2);
                assert_eq!(actual, count);
            }
            other => panic!("expected missing-body arity error, got {other:?}"),
        }
    }
    match evaluate(args(json!([7, null])), &ctx) {
        Err(OrbitalError::Eval(EvalError::InvalidArgument(message))) => {
            assert!(message.contains("[name value] pairs"), "{message}");
        }
        other => panic!("expected bindings-shape error, got {other:?}"),
    }
}
