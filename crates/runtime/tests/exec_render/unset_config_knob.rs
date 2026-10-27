// An unset `@config` knob in a render prop is an absent prop, not `null` (twin of the JS evaluator; ui `Chat.realtime-chat` lost its Send button to `placeholder: null`).

use serde_json::{json, Value as Json};
use std::sync::Arc;
use {
    orbital_core_base::client_effect::ClientEffect,
    orbital_core_base::effect_types::RuntimeEnvironment,
    orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::value::Value,
    orbital_core_runtime::effects::response::EventResponse,
};
use {
    orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache},
    orbital_core_runtime::kernel::RuntimeKernel,
};

fn response(config: Json, slot: &str, props: Json) -> EventResponse {
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
                        "effects": [["render-ui", slot, props]] }]
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
    EventResponse::from_process_results(&outcomes, "T", "INIT")
}

fn painted_slots(config: Json, slot: &str) -> Vec<String> {
    response(
        config,
        slot,
        json!({ "type": "typography", "content": "x" }),
    )
    .client_effects()
    .into_iter()
    .filter_map(|e| match e {
        ClientEffect::RenderUi { slot, .. } => Some(slot.clone()),
        _ => None,
    })
    .collect()
}

fn rendered(config: Json, props: Json) -> Json {
    let pattern = response(config, "main", props)
        .client_effects()
        .into_iter()
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

#[test]
fn a_slot_bound_to_a_knob_paints_into_the_knobs_value() {
    assert_eq!(
        painted_slots(
            json!({ "detailSlot": { "type": "slot", "default": "modal" } }),
            "@config.detailSlot"
        ),
        ["modal"]
    );
}

#[test]
fn control_a_literal_slot_paints_into_itself() {
    assert_eq!(painted_slots(json!({}), "main"), ["main"]);
}

#[test]
fn edge_a_slot_bound_to_an_unset_knob_paints_nothing() {
    assert!(painted_slots(
        json!({ "detailSlot": { "type": "slot" } }),
        "@config.detailSlot"
    )
    .is_empty());
}
