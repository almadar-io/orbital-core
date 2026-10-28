// A `listens` event written `Trait.EVENT` (or `Orbital.Trait.EVENT`) with no `source` is that source's event (twin of `parseListenSource`, `almadar-runtime/src/events/identity/routing.ts`; ui `game-keyboard-routing`).

use {orbital_core_runtime::entity_source::{EntitySource, InMemoryEntityCache}, orbital_core_runtime::kernel::RuntimeKernel};
use {orbital_core_base::schema::types::OrbitalSchema, orbital_core_base::effect_types::RuntimeEnvironment, orbital_core_base::value::Value};
use serde_json::json;
use std::sync::Arc;

fn steered(heard: &str) -> bool {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "Snake",
        "typeContractVersion": 1, "orbitals": [{
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

fn guarded(sent_key: &str, guard: serde_json::Value) -> bool {
    let schema: OrbitalSchema = serde_json::from_value(json!({
        "name": "Keyed",
        "typeContractVersion": 1, "orbitals": [{
            "name": "KeyedOrbital",
            "entity": { "name": "S", "persistence": "runtime", "fields": [{ "name": "id", "type": "string" }, { "name": "dir", "type": "string" }] },
            "traits": [
                { "name": "Play", "linkedEntity": "S",
                  "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
                      "transitions": [{ "from": "on", "to": "on", "event": "UP", "effects": [["emit", "TURN", { "key": sent_key }]] }] } },
                { "name": "Mechanic", "linkedEntity": "S",
                  "config": { "scope": { "type": "string", "default": "left" }, "name": { "type": "string", "default": "pad" } },
                  "listens": [{ "event": "TURN", "triggers": "STEER", "guard": guard, "source": { "kind": "any" } }],
                  "stateMachine": { "states": [{ "name": "on", "isInitial": true }],
                      "transitions": [{ "from": "on", "to": "on", "event": "STEER", "effects": [["set", "@entity.dir", "heard"]] }] } }
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
fn a_listen_guard_reads_the_listening_traits_own_config() {
    let guard = json!(["=", "@payload.key", ["str/concat", "@config.scope", "|", "@config.name"]]);
    assert!(guarded("left|pad", guard));
}

#[test]
fn control_a_listen_guard_on_config_refuses_another_key() {
    let guard = json!(["=", "@payload.key", ["str/concat", "@config.scope", "|", "@config.name"]]);
    assert!(!guarded("right|pad", guard));
}

#[test]
fn control_a_listen_guard_on_a_literal_key_still_delivers() {
    assert!(guarded("left|pad", json!(["=", "@payload.key", "left|pad"])));
}
