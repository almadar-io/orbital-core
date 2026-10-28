// An emit the dispatch itself consumed (its own arm for it, or a listener target) leaves marked `source.dispatched: true`, so a client fold does not run it again (G-ORB-275; twin of `TraitCascade` / `evaluateOrbitalEvent`).

use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_runtime::effects::response::EventResponse, orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::{json, Value as Json};
use std::sync::Arc;

fn emitted_dispatched(orbitals: Json) -> Option<bool> {
    let schema: OrbitalSchema =
        serde_json::from_value(json!({ "name": "App", "typeContractVersion": 1, "orbitals": orbitals })).unwrap();
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

/// An emit whose payload is `(quote x)` carries `x` as data: an operator-headed list inside it (a described
/// machine's guard) is never evaluated against the emitter's scope (orb site live-machine, 2026-10-19).
fn quoted_emit(env: RuntimeEnvironment) -> (Vec<String>, Option<Json>) {
    let described = json!({ "guard": ["array/some", "@entity.transitions", ["fn", "t", true]], "states": ["a"] });
    let body = orbital_core_base::schema::quote::encode_quote_body(&serde_json::from_value(described).unwrap());
    let schema: OrbitalSchema = serde_json::from_value(json!({ "name": "App", "typeContractVersion": 1, "orbitals": [orbital("Src", json!([source(json!([
        { "from": "idle", "to": "loading", "event": "GO", "effects": [["emit", "LOADED", { "result": ["quote", body] }]] }
    ]))]))] }))
    .unwrap();
    let k = RuntimeKernel::new(env, Arc::new(InMemoryEntityCache::new()) as Arc<dyn EntitySource>, Default::default());
    k.register_schema(schema).unwrap();
    let outcomes = k.dispatch("Source", None, "GO", Value::Null).unwrap();
    let response = EventResponse::from_process_results(&outcomes, "Source", "GO");
    let failures = outcomes.iter().flat_map(|(_, r)| r.effect_results.iter()).filter(|r| r.is_failed()).map(|r| r.error.clone().unwrap_or_default()).collect();
    let payload = response.emitted_events.iter().find(|e| e.event == "LOADED").map(|e| serde_json::to_value(&e.payload).unwrap());
    (failures, payload)
}

#[test]
fn a_quoted_emit_payload_is_carried_as_data_on_both_roles() {
    for env in [RuntimeEnvironment::Server, RuntimeEnvironment::Client] {
        let (failures, payload) = quoted_emit(env);
        assert!(failures.is_empty(), "{env:?}: {failures:?}");
        assert_eq!(payload.unwrap()["result"]["guard"][0], "array/some", "{env:?}");
    }
}
