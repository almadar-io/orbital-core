// An unset `@config` knob in a render prop is an absent prop, not `null` (twin of the JS evaluator; ui `Chat.realtime-chat` lost its Send button to `placeholder: null`).

use orbital_core::runtime::{EntitySource, InMemoryEntityCache, RuntimeKernel};
use orbital_core::{ClientEffect, EventResponse, OrbitalSchema, RuntimeEnvironment, Value};
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn rendered(config: Json, props: Json) -> Json {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "O",
            "entity": { "name": "E", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "T", "linkedEntity": "E",
                "config": config,
                "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                        "effects": [["render-ui", "main", props]] }]
                }
            }]
        }]
    }))
    .unwrap();
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Client,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(schema).unwrap();
    let outcomes = k.dispatch("T", None, "INIT", Value::Null).unwrap();
    let response = EventResponse::from_process_results(&outcomes, "T", "INIT");
    let pattern = response
        .client_effects
        .iter()
        .flatten()
        .find_map(|e| match e {
            ClientEffect::RenderUi { config, .. } => config.clone(),
            _ => None,
        })
        .expect("a render-ui effect");
    serde_json::to_value(pattern).unwrap()
}

#[test]
fn an_unset_config_knob_is_an_absent_prop() {
    let pattern = rendered(
        json!({ "hint": { "type": "string" } }),
        json!({ "type": "input-group", "placeholder": "@config.hint", "value": "x" }),
    );
    assert!(pattern.get("placeholder").is_none(), "{pattern}");
    assert_eq!(pattern["value"], "x");
}

#[test]
fn a_set_knob_renders_and_a_declared_null_default_stays_null() {
    let set = rendered(
        json!({ "hint": { "type": "string", "default": "Type" } }),
        json!({ "type": "input-group", "placeholder": "@config.hint" }),
    );
    assert_eq!(set["placeholder"], "Type");
    let declared_null = rendered(
        json!({ "hint": { "type": "string", "default": null } }),
        json!({ "type": "input-group", "placeholder": "@config.hint" }),
    );
    assert!(declared_null["placeholder"].is_null(), "{declared_null}");
}

#[test]
fn an_unset_knob_nested_in_children_is_absent_too() {
    let pattern = rendered(
        json!({ "hint": { "type": "string" } }),
        json!({ "type": "stack", "children": [{ "type": "input", "placeholder": "@config.hint" }] }),
    );
    assert!(
        pattern["children"][0].get("placeholder").is_none(),
        "{pattern}"
    );
}
