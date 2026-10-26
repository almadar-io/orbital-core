use super::*;

fn ctx_with_batch() -> BindingContext {
    let mut entity = ValueMap::default();
    entity.insert("batchId".to_string(), Value::string("b-7"));
    BindingContextBuilder::new()
        .entity(Value::object(entity))
        .build()
}

#[test]
fn call_service_key_resolves_from_the_entity_binding() {
    let effect = Effect::CallService {
        service: "llm".into(),
        method: "classifyMany".into(),
        args: Value::Null,
        key: Some("@entity.batchId".into()),
        emit: None,
    };
    let Ok(Effect::CallService { key, .. }) =
        resolve_effect_bindings(effect, &ctx_with_batch())
    else {
        panic!("expected CallService");
    };
    assert_eq!(key.as_ref().and_then(Value::as_str), Some("b-7"));
}

#[test]
fn cancel_call_key_resolves_from_the_entity_binding() {
    let effect = Effect::CancelCall {
        key: Value::string("@entity.batchId"),
    };
    let Ok(Effect::CancelCall { key }) = resolve_effect_bindings(effect, &ctx_with_batch())
    else {
        panic!("expected CancelCall");
    };
    assert_eq!(key, Value::string("b-7"));
}

#[test]
fn a_cancel_call_on_a_bound_list_resolves_to_the_list_and_a_list_literal_evaluates() {
    let mut entity = ValueMap::default();
    entity.insert("batchId".to_string(), Value::string("b-7"));
    entity.insert(
        "keys".to_string(),
        Value::array(vec!["x".into(), "y".into()]),
    );
    let ctx = BindingContextBuilder::new()
        .entity(Value::object(entity))
        .build();
    let bound = Effect::CancelCall {
        key: Value::string("@entity.keys"),
    };
    let Ok(Effect::CancelCall { key }) = resolve_effect_bindings(bound, &ctx) else {
        panic!("expected CancelCall");
    };
    assert_eq!(key, Value::array(vec!["x".into(), "y".into()]));
    let literal = Effect::CancelCall {
        key: Value::array(vec!["list".into(), "p".into(), "@entity.batchId".into()]),
    };
    let Ok(Effect::CancelCall { key }) = resolve_effect_bindings(literal, &ctx) else {
        panic!("expected CancelCall");
    };
    assert_eq!(key, Value::array(vec!["p".into(), "b-7".into()]));
}

#[test]
fn a_literal_key_passes_through_unchanged() {
    let effect = Effect::CancelCall {
        key: Value::string("fixed"),
    };
    let Ok(Effect::CancelCall { key }) = resolve_effect_bindings(effect, &ctx_with_batch())
    else {
        panic!("expected CancelCall");
    };
    assert_eq!(key, Value::string("fixed"));
}
