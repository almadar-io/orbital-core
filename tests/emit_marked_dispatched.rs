// An emit the dispatch itself consumed (its own arm for it, or a listener target) leaves marked `source.dispatched: true`, so a client fold does not run it again (G-ORB-275; twin of `TraitCascade` / `evaluateOrbitalEvent`).

use orbital_core::runtime::{EntitySource, InMemoryEntityCache, RuntimeKernel};
use orbital_core::{EventResponse, OrbitalSchema, RuntimeEnvironment, Value};
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn emitted_dispatched(orbitals: Json) -> Option<bool> {
    let schema: OrbitalSchema =
        serde_json::from_value(json!({ "name": "App", "orbitals": orbitals })).unwrap();
    let k = RuntimeKernel::new(
        RuntimeEnvironment::Server,
        Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>,
        Default::default(),
    );
    k.register_schema(schema).unwrap();
    let outcomes = k.dispatch("Source", None, "GO", Value::Null).unwrap();
    let response = EventResponse::from_process_results(&outcomes, "Source", "GO");
    let emitted = response
        .emitted_events
        .iter()
        .find(|e| e.event == "LOADED")
        .expect("the LOADED emit");
    emitted.source.as_ref().and_then(|s| s.dispatched)
}

fn orbital(name: &str, traits: Json) -> Json {
    json!({
        "name": name,
        "entity": { "name": format!("{name}Row"), "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
        "traits": traits
    })
}

fn source(arms: Json) -> Json {
    json!({
        "name": "Source", "linkedEntity": "SrcRow",
        "stateMachine": {
            "states": [{ "name": "idle", "isInitial": true }, { "name": "loading" }, { "name": "ready" }],
            "transitions": arms
        }
    })
}

fn go_emit() -> Json {
    json!({ "from": "idle", "to": "loading", "event": "GO", "effects": [["emit", "LOADED", {}]] })
}

fn listener(heard_in_state: &str) -> Json {
    json!({
        "name": "Listener", "linkedEntity": "SrcRow",
        "listens": [{ "event": "LOADED", "triggers": "LOADED", "source": { "kind": "any" } }],
        "stateMachine": {
            "states": [{ "name": heard_in_state, "isInitial": true }],
            "transitions": [{ "from": heard_in_state, "to": heard_in_state, "event": "LOADED" }]
        }
    })
}

#[test]
fn an_emit_the_emitter_handles_from_its_new_state_is_marked_dispatched() {
    let arms = json!([go_emit(), { "from": "loading", "to": "ready", "event": "LOADED" }]);
    assert_eq!(
        emitted_dispatched(json!([orbital("Src", json!([source(arms)]))])),
        Some(true)
    );
}

#[test]
fn an_emit_a_listener_in_the_same_orbital_receives_is_marked_dispatched() {
    let ts = json!([source(json!([go_emit()])), listener("idle")]);
    assert_eq!(emitted_dispatched(json!([orbital("Src", ts)])), Some(true));
}

#[test]
fn an_emit_a_listener_in_another_orbital_receives_is_marked_dispatched() {
    assert_eq!(
        emitted_dispatched(json!([
            orbital("Src", json!([source(json!([go_emit()]))])),
            orbital("Other", json!([listener("idle")])),
        ])),
        Some(true)
    );
}

#[test]
fn control_an_emit_nobody_consumes_stays_unmarked() {
    assert_eq!(
        emitted_dispatched(json!([orbital("Src", json!([source(json!([go_emit()]))]))])),
        None
    );
}

#[test]
fn edge_an_arm_for_the_emit_from_another_state_does_not_consume_it() {
    let arms = json!([go_emit(), { "from": "ready", "to": "idle", "event": "LOADED" }]);
    assert_eq!(
        emitted_dispatched(json!([orbital("Src", json!([source(arms)]))])),
        None
    );
}
