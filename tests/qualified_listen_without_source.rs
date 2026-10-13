// A `listens` event written `Trait.EVENT` (or `Orbital.Trait.EVENT`) with no `source` is that source's event (twin of `parseListenSource`, `almadar-runtime/src/events/identity/routing.ts`; ui `game-keyboard-routing`).

use orbital_core::runtime::{EntitySource, InMemoryEntityCache, RuntimeKernel};
use orbital_core::{OrbitalSchema, RuntimeEnvironment, Value};
use serde_json::json;
use std::sync::Arc;

fn steered(heard: &str) -> bool {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "Snake",
        "orbitals": [{
            "name": "SnakeOrbital",
            "entity": { "name": "S", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "dir", "type": "string" }] },
            "traits": [
                { "name": "Play", "linkedEntity": "S",
                  "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
                      "transitions": [{ "from": "on", "to": "on", "event": "UP", "effects": [["emit", "TURN", { "dir": "up" }]] }] } },
                { "name": "Mechanic", "linkedEntity": "S",
                  "listens": [{ "event": heard, "triggers": "STEER" }],
                  "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
                      "transitions": [{ "from": "on", "to": "on", "event": "STEER", "effects": [["set", "@entity.dir", "@payload.dir"]] }] } }
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
    let outcomes = k.dispatch("Play", None, "UP", Value::Null).unwrap();
    outcomes.iter().any(|(t, r)| t == "Mechanic" && r.executed)
}

#[test]
fn a_trait_qualified_listen_hears_that_traits_emit() {
    assert!(steered("Play.TURN"));
}

#[test]
fn an_orbital_qualified_listen_hears_that_traits_emit() {
    assert!(steered("SnakeOrbital.Play.TURN"));
}

#[test]
fn a_wildcard_listen_hears_any_source() {
    assert!(steered("*.TURN"));
}

#[test]
fn control_a_listen_qualified_to_another_trait_hears_nothing() {
    assert!(!steered("Other.TURN"));
    assert!(!steered("SnakeOrbital.Other.TURN"));
}

#[test]
fn control_a_bare_listen_still_hears_any_source() {
    assert!(steered("TURN"));
}
