// Each client effect carries the transition that produced it: its own event key and from-state, a cascaded listener's included. Twin of `almadar-runtime/test/render-provenance.test.ts`.

use orbital_core::runtime::{EntitySource, InMemoryEntityCache, RuntimeKernel};
use orbital_core::{EventResponse, OrbitalSchema, RuntimeEnvironment, Value};
use serde_json::json;
use std::sync::Arc;

fn kernel() -> RuntimeKernel {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "provenance",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Note", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Form", "stateMachine": {
                    "states": [{ "name": "editing", "isInitial": true }, { "name": "saved" }],
                    "transitions": [
                        { "from": "editing", "to": "editing", "event": "INIT", "effects": [["render-ui", "main", { "type": "typography", "content": "form" }]] },
                        { "from": "editing", "to": "saved", "event": "SAVE", "effects": [["render-ui", "main", { "type": "alert", "message": "Saved" }], ["emit", "SAVED"]] }
                    ]
                } },
                { "name": "Catalog",
                  "listens": [{ "event": "SAVED", "triggers": "REFRESH", "source": { "kind": "trait", "trait": "Form" } }],
                  "stateMachine": {
                    "states": [{ "name": "idle", "isInitial": true }],
                    "transitions": [{ "from": "idle", "to": "idle", "event": "REFRESH", "effects": [["render-ui", "sidebar", { "type": "typography", "content": "list" }]] }]
                } }
            ]
        }]
    }))
    .unwrap();
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(schema).unwrap();
    k
}

fn response(event: &str) -> EventResponse {
    let k = kernel();
    let outcomes = k.dispatch("Form", None, event, Value::Null).unwrap();
    EventResponse::from_process_results(&outcomes, "Form", event)
}

fn tags(r: &EventResponse) -> Vec<(String, Option<String>, Option<String>)> {
    r.client_effects_by_trait
        .iter()
        .flatten()
        .map(|e| (e.trait_name.clone(), e.event.clone(), e.from_state.clone()))
        .collect()
}

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

#[test]
fn a_mount_render_is_tagged_with_init_and_its_source_state() {
    assert_eq!(
        tags(&response("INIT")),
        vec![("Form".into(), s("INIT"), s("editing"))]
    );
}

#[test]
fn a_cascaded_listeners_render_carries_its_own_triggered_event() {
    assert_eq!(
        tags(&response("SAVE")),
        vec![
            ("Form".into(), s("SAVE"), s("editing")),
            ("Catalog".into(), s("REFRESH"), s("idle")),
        ]
    );
}

#[test]
fn the_effect_itself_is_unchanged_and_stays_one_to_one_with_the_flat_list() {
    let r = response("SAVE");
    let by_trait: Vec<_> = r
        .client_effects_by_trait
        .iter()
        .flatten()
        .map(|e| e.effect.clone())
        .collect();
    assert_eq!(Some(by_trait), r.client_effects);
}

#[test]
fn a_blocked_event_renders_untagged() {
    let r = response("NO_SUCH_EVENT");
    assert!(
        tags(&r).iter().all(|(_, e, f)| e.is_none() && f.is_none()),
        "{:?}",
        tags(&r)
    );
}
