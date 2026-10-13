// An emit reaches its listeners once, even when the emitter handles the event itself: the emitter's own
// arm runs the event as a same-trait step, not as a second offer of it to every listener. A listener that
// has since moved on would otherwise take the event again. JS twin: `runTraitCascade` + the worklist in
// `evaluateOrbitalEvent`, `task-comments-scoped-and-nested.test.ts`.

use orbital_core::runtime::{EntitySource, InMemoryEntityCache, RuntimeKernel};
use orbital_core::{OrbitalSchema, RuntimeEnvironment, Value};
use serde_json::json;
use std::sync::Arc;

fn deliveries(listener_arm_from: &str) -> usize {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Detail", "linkedEntity": "Item",
                  "emits": [{ "event": "Loaded" }],
                  "stateMachine": {
                      "states": [{ "name": "loading", "isInitial": true }, { "name": "viewing" }],
                      "transitions": [
                          { "from": "loading", "to": "loading", "event": "INIT", "effects": [["emit", "Loaded", {}]] },
                          { "from": "loading", "to": "viewing", "event": "Loaded" }
                      ]
                  } },
                { "name": "Thread", "linkedEntity": "Item",
                  "listens": [{ "event": "Loaded", "triggers": "SET_ROOT", "source": { "kind": "trait", "trait": "Detail" } }],
                  "stateMachine": {
                      "states": [{ "name": "browsing", "isInitial": true }, { "name": "loading" }],
                      "transitions": [
                          { "from": listener_arm_from, "to": "loading", "event": "SET_ROOT" },
                          { "from": "loading", "to": "browsing", "event": "SET_ROOT" }
                      ]
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
    k.dispatch("Detail", None, "INIT", Value::Null)
        .unwrap()
        .iter()
        .filter(|(t, r)| t == "Thread" && r.executed)
        .count()
}

#[test]
fn a_listener_takes_a_handled_emit_once() {
    assert_eq!(deliveries("browsing"), 1);
}

#[test]
fn control_a_listener_of_an_unhandled_emit_also_takes_it_once() {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "App",
        "orbitals": [{
            "name": "Main",
            "entity": { "name": "Item", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }] },
            "traits": [
                { "name": "Detail", "linkedEntity": "Item", "emits": [{ "event": "Loaded" }],
                  "stateMachine": { "states": [{ "name": "idle", "isInitial": true }],
                      "transitions": [{ "from": "idle", "to": "idle", "event": "INIT", "effects": [["emit", "Loaded", {}]] }] } },
                { "name": "Thread", "linkedEntity": "Item",
                  "listens": [{ "event": "Loaded", "triggers": "SET_ROOT", "source": { "kind": "trait", "trait": "Detail" } }],
                  "stateMachine": { "states": [{ "name": "browsing", "isInitial": true }, { "name": "loading" }],
                      "transitions": [{ "from": "browsing", "to": "loading", "event": "SET_ROOT" },
                                      { "from": "loading", "to": "browsing", "event": "SET_ROOT" }] } }
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
    let ran = k
        .dispatch("Detail", None, "INIT", Value::Null)
        .unwrap()
        .iter()
        .filter(|(t, r)| t == "Thread" && r.executed)
        .count();
    assert_eq!(ran, 1);
}
