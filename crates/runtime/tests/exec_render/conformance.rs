//! EXEC-RENDER conformance: how a `render-ui` effect resolves where it paints.


use orbital_core_base::effect_types::{Effect, EffectOutcome};
use orbital_core_base::value::Value;
use orbital_core_runtime::config::KernelConfig;
use serde_json::{json, Value as Json};
use crate::support::TestKernel;

fn render_outcome(config: Json, slot: &str) -> EffectOutcome {
    let kernel = TestKernel::client().config(KernelConfig::default()).build(json!({
        "name": "App",
        "typeContractVersion": 1, "orbitals": [{
            "name": "O",
            "entity": { "name": "E", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "T", "linkedEntity": "E", "config": config,
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                        "effects": [["render-ui", slot, { "type": "typography", "content": "x" }]] }] } }]
        }]
    }));
    let outcomes = kernel.dispatch("T", None, "INIT", Value::Null).unwrap();
    outcomes
        .iter()
        .filter(|(t, _)| t == "T")
        .flat_map(|(_, r)| r.effect_results.iter())
        .find(|e| matches!(e.effect, Effect::RenderUi { .. }))
        .map(|e| e.outcome.clone())
        .expect("a render-ui effect result")
}

#[test]
fn exec_render_7_a_slot_binding_that_resolves_to_nothing_fails_the_effect() {
    let outcome = render_outcome(json!({ "detailSlot": { "type": "slot" } }), "@config.detailSlot");
    assert!(matches!(outcome, EffectOutcome::Failed), "{outcome:?}");
}

#[test]
fn exec_render_7_control_a_literal_slot_does_not_fail() {
    let outcome = render_outcome(json!({}), "main");
    assert!(!matches!(outcome, EffectOutcome::Failed), "{outcome:?}");
}

use orbital_core_base::client_effect::ClientEffect;
use orbital_core_base::schema::types::OrbitalSchema;
use orbital_core_runtime::effects::response::EventResponse;

fn nav_schema(pages: Json) -> OrbitalSchema {
    serde_json::from_value(json!({
        "name": "NavApp",
        "typeContractVersion": 1, "orbitals": [{
            "name": "ShellOrbital",
            "entity": { "name": "Shell", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{
                "name": "Layout",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT",
                        "effects": [["render-ui", "main", { "type": "stack", "items": "@pages" }]] }] }
            }],
            "pages": pages
        }]
    }))
    .unwrap()
}

fn layout_response(schema: OrbitalSchema) -> EventResponse {
    let kernel = TestKernel::server().register(schema);
    let outcomes = kernel.dispatch("Layout", None, "INIT", Value::Null).unwrap();
    EventResponse::from_process_results(&outcomes, "Layout", "INIT")
}

fn pages_items(schema: OrbitalSchema) -> Json {
    let response = layout_response(schema);
    let effects = response.client_effects();
    let effect = effects
        .iter()
        .find(|e| matches!(e, ClientEffect::RenderUi { .. }))
        .expect("a render-ui effect");
    serde_json::to_value(effect).expect("effect serializes")[2]["items"].clone()
}

/// EXEC-RENDER-2: `@pages` is resolved by the kernel to the root nav entries that declare `@label`, deduped by path.
#[test]
fn exec_render_2_pages_resolves_to_labeled_root_pages_deduped_by_path() {
    let items = pages_items(nav_schema(json!([
        { "name": "HomePage", "path": "/", "traits": [{ "ref": "Layout" }] },
        { "name": "AboutPage", "path": "/about", "label": "About", "icon": "info", "traits": [] },
        { "name": "AboutAgain", "path": "/about", "label": "About again", "traits": [] }
    ])));
    assert_eq!(items, json!([{ "href": "/about", "label": "About", "icon": "info" }]), "labeled page only, first of a duplicated path");
}

#[test]
fn exec_render_2_control_with_no_labeled_page_pages_is_an_empty_list() {
    let items = pages_items(nav_schema(json!([
        { "name": "HomePage", "path": "/", "traits": [{ "ref": "Layout" }] }
    ])));
    assert_eq!(items, json!([]), "no labeled page, empty list");
}

fn render_effect_wire(extra: Json) -> Vec<Json> {
    let mut effects = vec![json!(["render-ui", "main", { "type": "typography", "content": "x" }])];
    effects.extend(extra.as_array().cloned().unwrap_or_default());
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "Wire",
        "typeContractVersion": 1, "orbitals": [{
            "name": "O",
            "entity": { "name": "E", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [{ "name": "T", "linkedEntity": "E",
                "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": effects }] } }]
        }]
    }))
    .unwrap();
    let kernel = TestKernel::server().register(schema);
    let outcomes = kernel.dispatch("T", None, "INIT", Value::Null).unwrap();
    EventResponse::from_process_results(&outcomes, "T", "INIT")
        .client_effects()
        .iter()
        .map(|e| serde_json::to_value(e).expect("effect serializes"))
        .collect()
}

/// EXEC-RENDER-6: a `render-ui` effect is exactly `["render-ui", slot, pattern]`, no trailing null placeholders.
#[test]
fn exec_render_6_the_render_ui_effect_is_a_three_element_tuple_without_padding() {
    let wire = render_effect_wire(json!([["navigate", "/x"]]));
    let render = wire.iter().find(|e| e[0] == "render-ui").expect("a render-ui tuple");
    let tuple = render.as_array().expect("tuple is an array");
    assert_eq!(tuple.len(), 3, "no trailing placeholders: {render}");
    assert_eq!(tuple[1], json!("main"));
    assert_eq!(tuple[2]["type"], json!("typography"), "the third element is the pattern");
}

#[test]
fn exec_render_6_control_a_navigate_effect_without_options_is_not_padded_either() {
    let wire = render_effect_wire(json!([["navigate", "/x"]]));
    let nav = wire.iter().find(|e| e[0] == "navigate").expect("a navigate tuple");
    assert_eq!(nav.as_array().map(Vec::len), Some(2), "arity follows what is present: {nav}");
}
