use super::*;

fn ctx_with_batch() -> BindingContext {
    let mut entity = ValueMap::default();
    entity.insert("batchId".to_string(), Value::String("b-7".to_string()));
    BindingContextBuilder::new()
        .entity(Value::Object(entity))
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
        key: Value::String("@entity.batchId".into()),
    };
    let Ok(Effect::CancelCall { key }) = resolve_effect_bindings(effect, &ctx_with_batch())
    else {
        panic!("expected CancelCall");
    };
    assert_eq!(key, Value::String("b-7".into()));
}

#[test]
fn a_cancel_call_on_a_bound_list_resolves_to_the_list_and_a_list_literal_evaluates() {
    let mut entity = ValueMap::default();
    entity.insert("batchId".to_string(), Value::String("b-7".to_string()));
    entity.insert(
        "keys".to_string(),
        Value::Array(vec!["x".into(), "y".into()]),
    );
    let ctx = BindingContextBuilder::new()
        .entity(Value::Object(entity))
        .build();
    let bound = Effect::CancelCall {
        key: Value::String("@entity.keys".into()),
    };
    let Ok(Effect::CancelCall { key }) = resolve_effect_bindings(bound, &ctx) else {
        panic!("expected CancelCall");
    };
    assert_eq!(key, Value::Array(vec!["x".into(), "y".into()]));
    let literal = Effect::CancelCall {
        key: Value::Array(vec!["list".into(), "p".into(), "@entity.batchId".into()]),
    };
    let Ok(Effect::CancelCall { key }) = resolve_effect_bindings(literal, &ctx) else {
        panic!("expected CancelCall");
    };
    assert_eq!(key, Value::Array(vec!["p".into(), "b-7".into()]));
}

#[test]
fn a_literal_key_passes_through_unchanged() {
    let effect = Effect::CancelCall {
        key: Value::String("fixed".into()),
    };
    let Ok(Effect::CancelCall { key }) = resolve_effect_bindings(effect, &ctx_with_batch())
    else {
        panic!("expected CancelCall");
    };
    assert_eq!(key, Value::String("fixed".into()));
}
