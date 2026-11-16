//! A binding that holds no value, written in an effect's data (G-ORBITAL-CORE-207). The owner ruled 2026-11-16 that it is
//! null everywhere (EXEC-EVAL-9). The kernel still leaves the field out of an `emit` payload and keeps an unset knob's
//! text; the ignored tests are the rule to turn on once the programs that rely on that are migrated.


use crate::support::TestKernel;
use orbital_core_base::effect_types::Effect;
use orbital_core_base::json_value::from_json_value;
use orbital_core_base::value::Value;
use orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache};
use serde_json::json;
use std::sync::Arc;

fn program(effects: serde_json::Value) -> serde_json::Value {
    json!({
        "name": "App", "typeContractVersion": 1, "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "persistent", "collection": "notes", "fields": [
                { "name": "id", "type": "string" }, { "name": "title", "type": "string" }] },
            "traits": [{
                "name": "Notes", "linkedEntity": "Note",
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "GO", "effects": effects }]
                }
            }]
        }]
    })
}

fn value(json: serde_json::Value) -> Value {
    from_json_value(&json).expect("a value")
}

/// The effects a `GO` with `payload` ran, as the kernel reports them.
fn ran(effects: serde_json::Value, payload: serde_json::Value) -> Vec<Effect> {
    let kernel = TestKernel::server().build(program(effects));
    let result = kernel.process_event("Notes", Some("singleton"), "GO", value(payload)).expect("the step runs");
    result.effect_results.into_iter().map(|r| r.effect).collect()
}

fn emitted(effects: serde_json::Value, payload: serde_json::Value) -> Value {
    ran(effects, payload)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Emit { payload, .. } => Some(payload),
            _ => None,
        })
        .expect("an emit ran")
}

fn emit() -> serde_json::Value {
    json!([["emit", "SEEN", { "a": "@payload.title", "c": ["default", "@payload.title", "none"], "d": ["@payload.title"] }]])
}

#[test]
fn control_an_emit_carries_a_field_whose_binding_holds_a_value() {
    assert_eq!(emitted(emit(), json!({ "title": "t" })), value(json!({ "a": "t", "c": "t", "d": ["t"] })));
}

#[test]
#[ignore = "G-ORBITAL-CORE-207: the rule after the programs are migrated; today the field is left out or the text is kept"]
fn an_emit_field_whose_binding_holds_no_value_is_null() {
    assert_eq!(emitted(emit(), json!({})), value(json!({ "a": null, "c": "none", "d": [null] })));
}

#[test]
#[ignore = "G-ORBITAL-CORE-207: the rule after the programs are migrated; today the field is left out or the text is kept"]
fn an_emit_field_bound_to_an_unset_knob_is_null() {
    let effects = json!([["emit", "SEEN", { "a": "@config.unset", "b": 1 }]]);
    assert_eq!(emitted(effects, json!({})), value(json!({ "a": null, "b": 1 })));
}

#[test]
#[ignore = "G-ORBITAL-CORE-207: the rule after the programs are migrated; today the field is left out or the text is kept"]
fn a_navigate_to_an_unset_knob_fails_and_never_goes_to_the_knobs_own_text() {
    let kernel = TestKernel::server().build(program(json!([["navigate", "@config.unset"]])));
    let result = kernel.process_event("Notes", Some("singleton"), "GO", value(json!({}))).expect("the step runs");
    let navigations: Vec<_> = result.effect_results.iter().filter(|r| matches!(r.effect, Effect::Navigate { .. })).collect();
    assert!(navigations.iter().all(|r| r.error.as_deref().is_some_and(|e| e.contains("not text"))), "{navigations:?}");
    assert!(!navigations.is_empty());
}

#[test]
fn a_navigate_param_whose_binding_holds_no_value_is_null() {
    let params = ran(json!([["navigate", "/x", { "id": "@payload.title" }]]), json!({}))
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Navigate { params, .. } => params,
            _ => None,
        })
        .expect("a navigate ran");
    assert_eq!(params.get("id"), Some(&Value::Null), "{params:?}");
}

#[test]
fn a_persist_writes_null_for_a_binding_the_payload_does_not_carry() {
    let cache = Arc::new(InMemoryEntityCache::new());
    let kernel = TestKernel::server()
        .source(cache.clone() as Arc<dyn EntitySource>)
        .build(program(json!([["persist", "create", "Note", { "id": "n1", "title": "@payload.title" }]])));
    kernel.process_event("Notes", Some("singleton"), "GO", value(json!({}))).expect("the step runs");
    let row = cache.read("Note", "n1").expect("the row is written");
    assert_eq!(row.get("title"), Some(&Value::Null), "{row:?}");
}

#[test]
fn control_a_persist_writes_the_value_a_binding_carries() {
    let cache = Arc::new(InMemoryEntityCache::new());
    let kernel = TestKernel::server()
        .source(cache.clone() as Arc<dyn EntitySource>)
        .build(program(json!([["persist", "create", "Note", { "id": "n1", "title": "@payload.title" }]])));
    kernel.process_event("Notes", Some("singleton"), "GO", value(json!({ "title": "t" }))).expect("the step runs");
    let row = cache.read("Note", "n1").expect("the row is written");
    assert_eq!(row.get("title"), Some(&Value::string("t")), "{row:?}");
}
